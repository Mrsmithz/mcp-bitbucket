use super::{Bitbucket, Config, RequestKind};
use crate::domain::{Page, Repository, SearchRepositoriesInput, ToolError, validate_segment};
use reqwest::{Method, Response, header};
use serde::Deserialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use url::Url;

const MAX_QUERY_BYTES: usize = 256;
const MAX_CURSOR_BYTES: usize = 64;
const MAX_UPSTREAM_PAGE: usize = 50;
const MAX_RETAINED_CURSORS: usize = 256;
static NEXT_NAMESPACE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(super) struct CursorRegistry(Arc<Mutex<CursorState>>);

struct CursorState {
    namespace: String,
    next_id: u64,
    entries: HashMap<String, Cursor>,
    order: VecDeque<String>,
}

#[derive(Clone)]
struct Cursor {
    query: String,
    pagelen: u8,
    next: Url,
}

impl CursorRegistry {
    pub(super) fn new() -> Self {
        Self(Arc::new(Mutex::new(CursorState {
            namespace: format!(
                "{:x}-{:x}-{:x}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                NEXT_NAMESPACE.fetch_add(1, Ordering::Relaxed),
            ),
            next_id: 1,
            entries: HashMap::new(),
            order: VecDeque::new(),
        })))
    }

    fn insert(&self, query: String, pagelen: u8, next: Url) -> String {
        let mut state = self.0.lock().expect("repository cursor lock poisoned");
        let token = format!("r{}-{:x}", state.namespace, state.next_id);
        state.next_id = state.next_id.wrapping_add(1);
        if state.entries.len() == MAX_RETAINED_CURSORS
            && let Some(oldest) = state.order.pop_front()
        {
            state.entries.remove(&oldest);
        }
        state.order.push_back(token.clone());
        state.entries.insert(
            token.clone(),
            Cursor {
                query,
                pagelen,
                next,
            },
        );
        token
    }

    fn get(&self, token: &str) -> Option<Cursor> {
        self.0
            .lock()
            .expect("repository cursor lock poisoned")
            .entries
            .get(token)
            .cloned()
    }
}

#[derive(Deserialize)]
struct WireRepositoryPage {
    values: Vec<WireRepository>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct WireRepository {
    uuid: String,
    name: String,
    full_name: String,
    workspace: Option<WireRepositoryWorkspace>,
    is_private: bool,
    links: WireRepositoryLinks,
}

#[derive(Deserialize)]
struct WireRepositoryWorkspace {
    slug: String,
}

#[derive(Deserialize)]
struct WireRepositoryLinks {
    html: WireRepositoryHtml,
}

#[derive(Deserialize)]
struct WireRepositoryHtml {
    href: String,
}

impl Bitbucket {
    pub async fn search_repositories(
        &self,
        input: SearchRepositoriesInput,
    ) -> Result<Page<Repository>, ToolError> {
        let raw_query = input.query.unwrap_or_default();
        if raw_query.len() > MAX_QUERY_BYTES {
            return Err(ToolError::Invalid("query must be at most 256 bytes".into()));
        }
        let query = raw_query.to_lowercase();
        let pagelen = input.pagelen.unwrap_or(MAX_UPSTREAM_PAGE as u8);
        if !(1..=MAX_UPSTREAM_PAGE as u8).contains(&pagelen) {
            return Err(ToolError::Invalid(
                "pagelen must be between 1 and 50".into(),
            ));
        }

        let mut url = self.url(&["repositories", self.config.workspace.0.as_str()])?;
        if let Some(token) = input.page.as_deref() {
            if token.is_empty() || token.len() > MAX_CURSOR_BYTES {
                return Err(ToolError::Invalid(
                    "page is invalid or expired; restart repository search".into(),
                ));
            }
            let cursor = self.cursors.get(token).ok_or_else(|| {
                ToolError::Invalid("page is invalid or expired; restart repository search".into())
            })?;
            if cursor.query != query || cursor.pagelen != pagelen {
                return Err(ToolError::Invalid(
                    "page belongs to a different repository query or page size".into(),
                ));
            }
            url = cursor.next;
        } else {
            url.query_pairs_mut()
                .append_pair("pagelen", &pagelen.to_string());
        }

        validate_repository_url(&self.config, &url, pagelen, None)?;
        let response = self.send_repository_get(url, pagelen).await?;
        let response_url = response.url().clone();
        let bytes = Self::bounded_body(response, RequestKind::Read).await?;
        let page: WireRepositoryPage =
            serde_json::from_slice(&bytes).map_err(|_| ToolError::Malformed)?;
        if page.values.len() > pagelen as usize {
            return Err(ToolError::Malformed);
        }
        let next = page
            .next
            .map(|value| {
                Url::parse(&value)
                    .map_err(|_| ToolError::Malformed)
                    .and_then(|next| {
                        validate_repository_url(&self.config, &next, pagelen, None)
                            .map_err(|_| ToolError::Malformed)?;
                        validate_repository_progress(&response_url, &next)
                            .map_err(|_| ToolError::Malformed)?;
                        Ok(next)
                    })
            })
            .transpose()?;
        let mut values = Vec::with_capacity(page.values.len());
        for repository in page.values {
            let repository = repository.into_domain(&self.config)?;
            if query.is_empty()
                || repository.name.to_lowercase().contains(&query)
                || repository.slug.to_lowercase().contains(&query)
            {
                values.push(repository);
            }
        }
        let next_page = next.map(|next| self.cursors.insert(query, pagelen, next));
        Ok(Page {
            values,
            next_page,
            truncated: false,
        })
    }

    async fn send_repository_get(&self, mut url: Url, pagelen: u8) -> Result<Response, ToolError> {
        for hop in 0..=super::MAX_REDIRECTS {
            let response = self
                .send_once(&Method::GET, &url, None, RequestKind::Read)
                .await?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            if hop == super::MAX_REDIRECTS {
                return Err(ToolError::UnsafeNavigation);
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(ToolError::UnsafeNavigation)?;
            let next = url
                .join(location)
                .map_err(|_| ToolError::UnsafeNavigation)?;
            validate_repository_url(&self.config, &next, pagelen, None)?;
            url = next;
        }
        Err(ToolError::UnsafeNavigation)
    }
}

impl WireRepository {
    fn into_domain(self, config: &Config) -> Result<Repository, ToolError> {
        if self.uuid.trim().is_empty()
            || self.uuid.len() > 128
            || self.uuid.bytes().any(|byte| byte.is_ascii_control())
            || self.name.trim().is_empty()
            || self.name.len() > 256
            || self.name.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ToolError::Malformed);
        }
        let (workspace, slug) = self.full_name.split_once('/').ok_or(ToolError::Malformed)?;
        if workspace != config.workspace.0
            || self.full_name.matches('/').count() != 1
            || self
                .workspace
                .as_ref()
                .is_some_and(|metadata| metadata.slug != config.workspace.0)
        {
            return Err(ToolError::Malformed);
        }
        validate_segment("slug", slug).map_err(|_| ToolError::Malformed)?;
        let url = Url::parse(&self.links.html.href).map_err(|_| ToolError::Malformed)?;
        let mut segments = url.path_segments().ok_or(ToolError::Malformed)?;
        let expected_workspace = segments.next().ok_or(ToolError::Malformed)?;
        let expected_slug = segments.next().ok_or(ToolError::Malformed)?;
        if url.scheme() != "https"
            || url.host_str() != Some("bitbucket.org")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || expected_workspace != workspace
            || expected_slug != slug
            || segments.next().is_some()
            || self.links.html.href.len() > 2048
        {
            return Err(ToolError::Malformed);
        }
        Ok(Repository {
            uuid: self.uuid,
            name: self.name,
            slug: slug.to_owned(),
            workspace: workspace.to_owned(),
            url: url.to_string(),
            is_private: self.is_private,
        })
    }
}

fn validate_repository_url(
    config: &Config,
    url: &Url,
    pagelen: u8,
    previous: Option<&Url>,
) -> Result<(), ToolError> {
    let root = &config.root;
    let expected_path = format!(
        "{}/repositories/{}",
        root.path().trim_end_matches('/'),
        config.workspace.0
    );
    if url.scheme() != root.scheme()
        || url.host_str() != root.host_str()
        || url.port_or_known_default() != root.port_or_known_default()
        || url.path() != expected_path
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(ToolError::UnsafeNavigation);
    }
    let mut size = None;
    let mut state = HashMap::new();
    for (key, value) in url.query_pairs() {
        let key = key.as_ref();
        let value = value.as_ref();
        match key {
            "pagelen" if size.is_none() && value == pagelen.to_string() => {
                size = Some(value.to_owned());
            }
            "page" | "after" | "before" | "cursor"
                if !value.is_empty() && value.len() <= 4096 && !state.contains_key(key) =>
            {
                state.insert(key.to_owned(), value.to_owned());
            }
            _ => return Err(ToolError::UnsafeNavigation),
        }
    }
    if size.is_none() || (previous.is_some() && state.is_empty()) {
        return Err(ToolError::UnsafeNavigation);
    }
    if let Some(previous) = previous {
        validate_repository_progress(previous, url)?;
    }
    Ok(())
}

fn validate_repository_progress(previous: &Url, next: &Url) -> Result<(), ToolError> {
    let state = |url: &Url| -> HashMap<String, String> {
        url.query_pairs()
            .filter(|(key, _)| matches!(key.as_ref(), "page" | "after" | "before" | "cursor"))
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect()
    };
    let old = state(previous);
    let new = state(next);
    if new.is_empty() || old == new {
        return Err(ToolError::UnsafeNavigation);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitbucket::TlsVerification;
    use serde_json::json;

    fn config() -> Config {
        Config::new(
            "a@b.test",
            "secret",
            "workspace",
            "https://api.bitbucket.org/2.0",
            TlsVerification::Verify,
        )
        .unwrap()
    }

    #[test]
    fn continuation_urls_are_bound_to_the_exact_repository_endpoint_and_page_size() {
        let config = config();
        let first = Url::parse(
            "https://api.bitbucket.org/2.0/repositories/workspace?pagelen=10&page=opaque",
        )
        .unwrap();
        assert!(validate_repository_url(&config, &first, 10, None).is_ok());
        for bad in [
            "https://evil.invalid/2.0/repositories/workspace?pagelen=10&page=opaque",
            "https://api.bitbucket.org.evil/2.0/repositories/workspace?pagelen=10&page=opaque",
            "https://api.bitbucket.org/2.0/repositories/other?pagelen=10&page=opaque",
            "https://api.bitbucket.org/2.0/repositories/workspace/pullrequests?pagelen=10&page=opaque",
            "https://api.bitbucket.org/2.0/repositories/workspace?pagelen=10&page=opaque&role=member",
            "https://user@api.bitbucket.org/2.0/repositories/workspace?pagelen=10&page=opaque",
            "https://api.bitbucket.org/2.0/repositories/workspace?pagelen=11&page=opaque",
        ] {
            assert!(
                validate_repository_url(&config, &Url::parse(bad).unwrap(), 10, None).is_err(),
                "{bad}"
            );
        }
        let changed = Url::parse(
            "https://api.bitbucket.org/2.0/repositories/workspace?pagelen=10&page=opaque",
        )
        .unwrap();
        assert!(validate_repository_url(&config, &changed, 10, Some(&first)).is_err());
        let opaque = Url::parse(
            "https://api.bitbucket.org/2.0/repositories/workspace?pagelen=10&after=opaque",
        )
        .unwrap();
        assert!(validate_repository_url(&config, &opaque, 10, Some(&first)).is_ok());
        assert!(validate_repository_progress(&first, &first).is_err());
        for bad in [
            "https://api.bitbucket.org/2.0/repositories/workspace?pagelen=10&after=",
            "https://api.bitbucket.org/2.0/repositories/workspace?pagelen=10&after=one&after=two",
            "https://api.bitbucket.org/2.0/repositories/workspace?pagelen=10&role=member",
        ] {
            assert!(validate_repository_url(&config, &Url::parse(bad).unwrap(), 10, None).is_err());
        }
        let start =
            Url::parse("https://api.bitbucket.org/2.0/repositories/workspace?pagelen=10").unwrap();
        assert!(validate_repository_url(&config, &first, 10, Some(&start)).is_ok());
    }

    #[test]
    fn repository_wire_shape_requires_workspace_slug_and_safe_html_url() {
        let config = config();
        let good: WireRepository = serde_json::from_value(json!({
            "uuid":"{stable-id}","name":"Public Project","full_name":"workspace/project",
            "is_private":false,"links":{"html":{"href":"https://bitbucket.org/workspace/project"}}
        }))
        .unwrap();
        let value = good.into_domain(&config).unwrap();
        assert_eq!(value.slug, "project");
        assert!(!value.is_private);
        let wrong = serde_json::from_value::<WireRepository>(json!({
            "uuid":"{stable-id}","name":"Project","full_name":"other/project",
            "is_private":true,"links":{"html":{"href":"https://bitbucket.org/other/project"}}
        }))
        .unwrap()
        .into_domain(&config);
        assert!(wrong.is_err());
        let wrong_workspace_metadata = serde_json::from_value::<WireRepository>(json!({
            "uuid":"{stable-id}","name":"Project","full_name":"workspace/project",
            "workspace":{"slug":"other"},"is_private":false,
            "links":{"html":{"href":"https://bitbucket.org/workspace/project"}}
        }))
        .unwrap()
        .into_domain(&config);
        assert!(wrong_workspace_metadata.is_err());
    }

    #[test]
    fn cursor_registry_replays_without_consuming_and_evicts_oldest_at_bound() {
        let registry = CursorRegistry::new();
        let next = Url::parse(
            "https://api.bitbucket.org/2.0/repositories/workspace?pagelen=1&page=opaque",
        )
        .unwrap();
        let first = registry.insert("q".into(), 1, next.clone());
        assert_eq!(registry.get(&first).unwrap().next, next);
        assert_eq!(registry.get(&first).unwrap().next, next);
        for _ in 0..MAX_RETAINED_CURSORS {
            registry.insert("q".into(), 1, next.clone());
        }
        assert!(registry.get(&first).is_none());
        assert_eq!(
            registry.0.lock().unwrap().entries.len(),
            MAX_RETAINED_CURSORS
        );
    }
}
