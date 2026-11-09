use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use mcp_bitbucket::bitbucket::{Bitbucket, Config, TlsVerification};
use mcp_bitbucket::domain::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

type RequestRecord = (String, String, Value, Option<String>);
#[derive(Clone)]
struct Fixture {
    seen: Arc<Mutex<Vec<RequestRecord>>>,
    continuation: &'static str,
    redirect: bool,
}

async fn respond(
    State(fixture): State<Fixture>,
    uri: axum::http::Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let query = uri.query().unwrap_or("").to_owned();
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let json_body = serde_json::from_slice(&body).unwrap_or(Value::Null);
    fixture.seen.lock().unwrap().push((
        method.to_string(),
        format!("{path}?{query}"),
        json_body,
        auth,
    ));
    if method != Method::GET || path != "/proxy/2.0/repositories/w" {
        return (StatusCode::NOT_FOUND, "unexpected fixture request").into_response();
    }
    let params: std::collections::HashMap<_, _> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let continuation_key = fixture.continuation.split('=').next().unwrap();
    if params.contains_key(continuation_key) {
        if fixture.redirect {
            let host = headers
                .get("host")
                .and_then(|value| value.to_str().ok())
                .unwrap();
            return Json(json!({"values":[],"next":format!("http://{host}{path}?{query}")}))
                .into_response();
        }
        return Json(json!({"values":[{
            "uuid":"{later-id}","name":"Later Match","full_name":"w/later-repo","is_private":true,
            "links":{"html":{"href":"https://bitbucket.org/w/later-repo"}}
        }]}))
        .into_response();
    }
    if fixture.redirect {
        return Response::builder()
            .status(StatusCode::FOUND)
            .header(
                "location",
                "/proxy/2.0/repositories/w?pagelen=50&cursor=opaque",
            )
            .body(Body::empty())
            .unwrap();
    }
    let values = json!([
        {"uuid":"{public-id}","name":r#"Release "A\B" [tool]+"#,"full_name":"w/public-tool","is_private":false,"links":{"html":{"href":"https://bitbucket.org/w/public-tool"}}},
        {"uuid":"{private-id}","name":"Private Project","full_name":"w/private-project","is_private":true,"links":{"html":{"href":"https://bitbucket.org/w/private-project"}}}
    ]);
    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap();
    let continuation = fixture.continuation;
    Json(json!({"values":values,"next":format!("http://{host}/proxy/2.0/repositories/w?pagelen=50&{continuation}")})).into_response()
}

async fn client(
    continuation: &'static str,
    redirect: bool,
) -> (Bitbucket, Arc<Mutex<Vec<RequestRecord>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new().fallback(any(respond)).with_state(Fixture {
        seen: seen.clone(),
        continuation,
        redirect,
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = Config::new(
        "a@b.test",
        "token-secret",
        "w",
        &format!("http://{addr}/proxy/2.0"),
        TlsVerification::Verify,
    )
    .unwrap();
    (Bitbucket::new(config).unwrap(), seen)
}

fn search(
    query: Option<&str>,
    page: Option<String>,
    pagelen: Option<u8>,
) -> SearchRepositoriesInput {
    SearchRepositoriesInput {
        query: query.map(str::to_owned),
        page,
        pagelen,
    }
}

#[tokio::test]
async fn repository_search_matches_literal_name_or_slug_and_validates_before_http() {
    let (api, seen) = client("page=opaque", false).await;
    for input in [
        search(Some(&"é".repeat(129)), None, None),
        search(None, None, Some(0)),
    ] {
        assert!(matches!(
            api.search_repositories(input).await,
            Err(ToolError::Invalid(_))
        ));
    }
    assert!(
        seen.lock().unwrap().is_empty(),
        "invalid search input reached HTTP"
    );

    let all = api
        .search_repositories(search(None, None, None))
        .await
        .unwrap();
    assert_eq!(all.values.len(), 2);
    assert_eq!(all.values[0].slug, "public-tool");
    assert!(!all.values[0].is_private);
    assert!(all.values[1].is_private);
    assert!(all.next_page.is_some());

    let quoted = api
        .search_repositories(search(Some(r#""a\b" [TOOL]+"#), None, None))
        .await
        .unwrap();
    assert_eq!(quoted.values.len(), 1);
    assert_eq!(quoted.values[0].name, r#"Release "A\B" [tool]+"#);
    let by_slug = api
        .search_repositories(search(Some("PUBLIC-TOOL"), None, None))
        .await
        .unwrap();
    assert_eq!(
        by_slug
            .values
            .iter()
            .map(|repo| repo.slug.as_str())
            .collect::<Vec<_>>(),
        ["public-tool"]
    );
    for needle in ["w", ".*", "nomatch", " PUBLIC-TOOL "] {
        let result = api
            .search_repositories(search(Some(needle), None, None))
            .await
            .unwrap();
        assert!(
            result.values.is_empty(),
            "query {needle:?} unexpectedly matched"
        );
        assert!(
            result.next_page.is_some(),
            "empty filtered page lost upstream continuation"
        );
    }

    let records = seen.lock().unwrap();
    assert!(records.iter().all(|(_, path, _, auth)| {
        path.contains("pagelen=50")
            && !path.contains("q=")
            && auth
                .as_deref()
                .is_some_and(|value| value.starts_with("Basic "))
    }));
}

#[tokio::test]
async fn repository_cursor_binds_query_page_size_workspace_and_replays() {
    let (api, seen) = client("page=opaque", false).await;
    let first = api
        .search_repositories(search(Some("Later"), None, Some(50)))
        .await
        .unwrap();
    assert!(first.values.is_empty());
    let token = first.next_page.unwrap();
    let before_rejection = seen.lock().unwrap().len();
    for (query, page_size) in [("different", 50), ("Later", 1)] {
        assert!(matches!(
            api.search_repositories(search(Some(query), Some(token.clone()), Some(page_size)))
                .await,
            Err(ToolError::Invalid(_))
        ));
        assert_eq!(seen.lock().unwrap().len(), before_rejection);
    }
    let config = Config::new(
        "a@b.test",
        "token",
        "w",
        "http://127.0.0.1:1/proxy/2.0",
        TlsVerification::Verify,
    )
    .unwrap();
    let other = Bitbucket::new(config).unwrap();
    assert!(matches!(
        other
            .search_repositories(search(Some("Later"), Some(token.clone()), Some(50)))
            .await,
        Err(ToolError::Invalid(_))
    ));
    assert_eq!(seen.lock().unwrap().len(), before_rejection);

    let later = api
        .search_repositories(search(Some("Later"), Some(token.clone()), Some(50)))
        .await
        .unwrap();
    assert_eq!(later.values[0].slug, "later-repo");
    let replay = api
        .search_repositories(search(Some("Later"), Some(token), Some(50)))
        .await
        .unwrap();
    assert_eq!(replay.values, later.values);
    let requests = seen.lock().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|(_, path, _, _)| path.contains("/repositories/w?"))
            .count(),
        before_rejection + 2
    );
}

#[tokio::test]
async fn repository_search_preserves_opaque_after_state_and_rejects_redirect_self_next() {
    let (api, seen) = client("after=opaque", false).await;
    let first = api
        .search_repositories(search(Some("Later"), None, None))
        .await
        .unwrap();
    let next = api
        .search_repositories(search(Some("Later"), first.next_page, None))
        .await
        .unwrap();
    assert_eq!(next.values[0].slug, "later-repo");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|(_, path, _, _)| path.ends_with("?pagelen=50&after=opaque"))
    );

    let (redirected, redirect_seen) = client("cursor=opaque", true).await;
    assert!(matches!(
        redirected
            .search_repositories(search(Some("Later"), None, None))
            .await,
        Err(ToolError::Malformed)
    ));
    assert_eq!(
        redirect_seen.lock().unwrap().len(),
        2,
        "self-next must be rejected before cursor registration"
    );
}
