use super::{Bitbucket, MAX_PAGE, MAX_REDIRECTS, RequestKind, bounded_diff, valid_page_token};
use crate::domain::{
    GetPullRequestFileDiffInput, ListPullRequestFilesInput, PullRequestFile, PullRequestFileDiff,
    ToolError, validate_pr, validate_repo_path, validate_segment,
};
use reqwest::{Method, Response, header};
use serde::Deserialize;
use url::Url;

const MAX_FILE_PAGE: usize = MAX_PAGE;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ComparisonKind {
    Diff,
    Diffstat,
}

impl ComparisonKind {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::Diffstat => "diffstat",
        }
    }
}

#[derive(Deserialize)]
struct WireFilePage {
    values: Vec<WireFile>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct WireFile {
    old: Option<WireFilePath>,
    new: Option<WireFilePath>,
    status: Option<String>,
    lines_added: Option<u64>,
    lines_removed: Option<u64>,
}

#[derive(Deserialize)]
struct WireFilePath {
    path: Option<String>,
}

impl Bitbucket {
    pub async fn list_pull_request_files(
        &self,
        input: ListPullRequestFilesInput,
    ) -> Result<crate::domain::Page<PullRequestFile>, ToolError> {
        validate_pr(input.pull_request_id)?;
        validate_segment("repo_slug", &input.repo_slug)?;
        validate_page(input.page.as_deref())?;
        let page_len = file_page_len(input.pagelen)?;
        let mut url = self
            .resolve_comparison(
                &input.repo_slug,
                input.pull_request_id,
                ComparisonKind::Diffstat,
            )
            .await?;
        set_query(&mut url, &[], None, input.page.as_deref(), Some(page_len));
        let response = self
            .comparison_response(
                url,
                ComparisonKind::Diffstat,
                None,
                input.page.as_deref(),
                Some(page_len),
            )
            .await?;
        let response_url = response.url().clone();
        let body = Bitbucket::bounded_body(response, RequestKind::Read).await?;
        let page: WireFilePage = serde_json::from_slice(&body).map_err(|_| ToolError::Malformed)?;
        let truncated = page.values.len() > MAX_FILE_PAGE;
        let values = page
            .values
            .into_iter()
            .take(MAX_FILE_PAGE)
            .map(|wire| {
                let old_path = wire.old.and_then(|side| side.path);
                let new_path = wire.new.and_then(|side| side.path);
                for path in old_path.iter().chain(new_path.iter()) {
                    validate_repo_path(path).map_err(|_| ToolError::Malformed)?;
                }
                if old_path.is_none() && new_path.is_none() {
                    return Err(ToolError::Malformed);
                }
                Ok(PullRequestFile {
                    old_path,
                    new_path,
                    status: wire.status,
                    lines_added: wire.lines_added,
                    lines_removed: wire.lines_removed,
                })
            })
            .collect::<Result<Vec<_>, ToolError>>()?;
        let next_page = page
            .next
            .as_deref()
            .map(|next| safe_next_page(self, &response_url, next, ComparisonKind::Diffstat))
            .transpose()?;
        Ok(crate::domain::Page {
            values,
            next_page,
            truncated,
        })
    }

    pub async fn get_pull_request_file_diff(
        &self,
        input: GetPullRequestFileDiffInput,
    ) -> Result<PullRequestFileDiff, ToolError> {
        validate_pr(input.pull_request_id)?;
        validate_segment("repo_slug", &input.repo_slug)?;
        validate_repo_path(&input.path)?;
        let url = self
            .resolve_comparison(
                &input.repo_slug,
                input.pull_request_id,
                ComparisonKind::Diff,
            )
            .await?;
        let mut url = url;
        set_query(&mut url, &[], Some(&input.path), None, None);
        let response = self
            .comparison_response(url, ComparisonKind::Diff, Some(&input.path), None, None)
            .await?;
        let (bytes, truncated) = bounded_diff(response).await?;
        let diff = String::from_utf8(bytes).map_err(|_| ToolError::Malformed)?;
        Ok(PullRequestFileDiff {
            path: input.path,
            diff,
            truncated,
        })
    }

    async fn resolve_comparison(
        &self,
        repo: &str,
        pull_request_id: u64,
        kind: ComparisonKind,
    ) -> Result<Url, ToolError> {
        let mut pr_url = self.target(repo, Some(pull_request_id))?;
        pr_url
            .path_segments_mut()
            .map_err(|_| ToolError::UnsafeNavigation)?
            .push(kind.endpoint());
        let response = self
            .send_once(&Method::GET, &pr_url, None, RequestKind::Read)
            .await?;
        if !response.status().is_redirection() {
            return Err(if response.status().is_success() {
                ToolError::UnsafeNavigation
            } else {
                ToolError::Rejected(response.status().as_u16())
            });
        }
        let url = redirect_target(self, &pr_url, &response)?;
        comparison_signature(self, &url, kind)?;
        Ok(url)
    }

    async fn comparison_response(
        &self,
        mut url: Url,
        kind: ComparisonKind,
        path_filter: Option<&str>,
        page: Option<&str>,
        page_len: Option<u8>,
    ) -> Result<Response, ToolError> {
        comparison_signature(self, &url, kind)?;
        let modifiers = query_modifiers(&url);
        let mut hops = 1;
        loop {
            let response = self
                .send_once(&Method::GET, &url, None, RequestKind::Read)
                .await?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            if hops == MAX_REDIRECTS {
                return Err(ToolError::UnsafeNavigation);
            }
            let next = redirect_target(self, &url, &response)?;
            comparison_signature(self, &next, kind)?;
            if has_unmatched_modifiers(&query_modifiers(&next), &modifiers) {
                return Err(ToolError::UnsafeNavigation);
            }
            url = next;
            set_query(&mut url, &modifiers, path_filter, page, page_len);
            hops += 1;
        }
    }
}

fn redirect_target(api: &Bitbucket, from: &Url, response: &Response) -> Result<Url, ToolError> {
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(ToolError::UnsafeNavigation)?;
    let target = from
        .join(location)
        .map_err(|_| ToolError::UnsafeNavigation)?;
    if !api.safe_get_redirect(from, &target) || target.fragment().is_some() {
        return Err(ToolError::UnsafeNavigation);
    }
    Ok(target)
}

fn comparison_signature(
    api: &Bitbucket,
    url: &Url,
    kind: ComparisonKind,
) -> Result<Vec<String>, ToolError> {
    let base: Vec<_> = api
        .config
        .root
        .path_segments()
        .ok_or(ToolError::UnsafeNavigation)?
        .filter(|segment| !segment.is_empty())
        .collect();
    let segments: Vec<_> = url
        .path_segments()
        .ok_or(ToolError::UnsafeNavigation)?
        .filter(|segment| !segment.is_empty())
        .collect();
    if !segments.starts_with(&base)
        || segments.len() < base.len() + 5
        || segments[base.len()] != "repositories"
        || validate_segment("workspace", segments[base.len() + 1]).is_err()
        || validate_segment("repo_slug", segments[base.len() + 2]).is_err()
        || segments[base.len() + 3] != kind.endpoint()
        || segments[base.len() + 4..]
            .iter()
            .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ToolError::UnsafeNavigation);
    }
    Ok(segments[base.len() + 1..]
        .iter()
        .map(|s| (*s).to_owned())
        .collect())
}

fn validate_page(page: Option<&str>) -> Result<(), ToolError> {
    if page.is_some_and(|page| !valid_page_token(page)) {
        return Err(ToolError::Invalid(
            "page must be an opaque safe page token".into(),
        ));
    }
    Ok(())
}

fn file_page_len(value: Option<u8>) -> Result<u8, ToolError> {
    match value {
        Some(0) => Err(ToolError::Invalid(
            "pagelen must be between 1 and 50".into(),
        )),
        Some(value) => Ok(value.min(MAX_FILE_PAGE as u8)),
        None => Ok(MAX_FILE_PAGE as u8),
    }
}

fn query_modifiers(url: &Url) -> Vec<(String, String)> {
    url.query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "path" | "page" | "pagelen"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn has_unmatched_modifiers(
    supplied: &[(String, String)],
    authoritative: &[(String, String)],
) -> bool {
    supplied
        .iter()
        .any(|pair| !authoritative.iter().any(|known| known == pair))
}

fn set_query(
    url: &mut Url,
    fallback_modifiers: &[(String, String)],
    path: Option<&str>,
    page: Option<&str>,
    page_len: Option<u8>,
) {
    let existing: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "path" | "page" | "pagelen"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let mut modifiers = existing;
    for (key, value) in fallback_modifiers {
        if let Some((_, current)) = modifiers.iter_mut().find(|(found, _)| found == key) {
            *current = value.clone();
        } else {
            modifiers.push((key.clone(), value.clone()));
        }
    }
    let mut query = url.query_pairs_mut();
    query.clear();
    for (key, value) in modifiers {
        query.append_pair(&key, &value);
    }
    if let Some(path) = path {
        query.append_pair("path", path);
    }
    if let Some(page) = page {
        query.append_pair("page", page);
    }
    if let Some(page_len) = page_len {
        query.append_pair("pagelen", &page_len.to_string());
    }
}

fn safe_next_page(
    api: &Bitbucket,
    current: &Url,
    next: &str,
    kind: ComparisonKind,
) -> Result<String, ToolError> {
    let next = Url::parse(next).map_err(|_| ToolError::Malformed)?;
    if !api.safe_get_redirect(current, &next)
        || comparison_signature(api, &next, kind)? != comparison_signature(api, current, kind)?
    {
        return Err(ToolError::Malformed);
    }
    let current_modifiers = query_modifiers(current);
    if has_unmatched_modifiers(&query_modifiers(&next), &current_modifiers) {
        return Err(ToolError::Malformed);
    }
    let mut values = next
        .query_pairs()
        .filter(|(key, _)| key == "page")
        .map(|(_, value)| value.into_owned());
    let token = values.next().ok_or(ToolError::Malformed)?;
    if values.next().is_some() || !valid_page_token(&token) {
        return Err(ToolError::Malformed);
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_page_length_is_bounded() {
        assert_eq!(file_page_len(None).unwrap(), 50);
        assert_eq!(file_page_len(Some(50)).unwrap(), 50);
        assert_eq!(file_page_len(Some(200)).unwrap(), 50);
        assert_eq!(file_page_len(Some(4)).unwrap(), 4);
        assert!(file_page_len(Some(0)).is_err());
    }

    #[test]
    fn semantic_modifiers_must_match_authoritative_pairs() {
        let original = vec![("topic".into(), "true".into())];
        assert!(!has_unmatched_modifiers(&original, &original));
        assert!(has_unmatched_modifiers(
            &[("topic".into(), "false".into())],
            &original
        ));
        assert!(has_unmatched_modifiers(
            &[("topic".into(), "true".into()), ("step".into(), "2".into())],
            &original
        ));
        assert!(!has_unmatched_modifiers(&[], &original));
    }

    #[test]
    fn filtered_query_preserves_modifiers_and_encodes_paths() {
        let mut url =
            Url::parse("https://example.test/repositories/w/r/diff/a..b?topic=true&path=stale")
                .unwrap();
        set_query(&mut url, &[], Some("src/日本語/[id]+name?.rs"), None, None);
        assert_eq!(
            url.query_pairs().find(|(key, _)| key == "topic").unwrap().1,
            "true"
        );
        assert_eq!(
            url.query_pairs().find(|(key, _)| key == "path").unwrap().1,
            "src/日本語/[id]+name?.rs"
        );
    }
}
