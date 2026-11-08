use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use mcp_bitbucket::bitbucket::{Bitbucket, Config};
use mcp_bitbucket::domain::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

type RequestRecord = (String, String, Value, Option<String>);
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<RequestRecord>>>);
async fn respond(
    State(seen): State<Seen>,
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
    seen.0.lock().unwrap().push((
        method.to_string(),
        format!("{path}?{query}"),
        json_body.clone(),
        auth,
    ));
    if path.ends_with("/pullrequests/1/diff") {
        return Response::builder()
            .status(StatusCode::FOUND)
            .header(
                "location",
                "/proxy/2.0/repositories/w/r/pullrequests/1/diff-final",
            )
            .body(Body::empty())
            .unwrap();
    }
    if path.ends_with("/pullrequests/3/diff") {
        let body = format!(
            "diff --git a/src/file.rs b/src/file.rs\n--- a/src/file.rs\n+++ b/src/file.rs\n@@ -1,1 +1,1 @@\n {}\n",
            "x".repeat(600_000)
        );
        return Response::builder()
            .status(200)
            .body(Body::from(body))
            .unwrap();
    }
    if path.ends_with("/pullrequests/9/diff") {
        return Response::builder()
            .status(200)
            .body(Body::from(
                "diff --git a/old b/new\nsimilarity index 100%\nrename from old\nrename to new\n",
            ))
            .unwrap();
    }
    if path.ends_with("/pullrequests/10/diff") {
        return Response::builder()
            .status(200)
            .body(Body::from("diff --git a/bin b/bin\nGIT binary patch\n"))
            .unwrap();
    }
    if path.ends_with("/pullrequests/11/diff") {
        return Response::builder()
            .status(200)
            .body(Body::from(
                "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ malformed\n",
            ))
            .unwrap();
    }
    if path.ends_with("/pullrequests/8/diff") {
        return Response::builder()
            .status(200)
            .body(Body::from(
                "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+last-line",
            ))
            .unwrap();
    }
    if path.ends_with("/pullrequests/4/diff") {
        return Response::builder().status(200).body(Body::from("diff --git a/src/日本語/[id].tsx b/src/日本語/[id].tsx\nindex abc..def 100644\n--- a/src/日本語/[id].tsx\n+++ b/src/日本語/[id].tsx\n@@ -1,1 +1,1 @@\n--- old SQL comment\n+new value\n")).unwrap();
    }
    if path.ends_with("/pullrequests/2/diff") {
        return Response::builder()
            .status(StatusCode::FOUND)
            .header("location", "https://example.invalid/steal")
            .body(Body::empty())
            .unwrap();
    }
    if path.ends_with("/diff-final") {
        return Response::builder().status(200).body(Body::from("diff --git a/src/file.rs b/src/file.rs\n--- a/src/file.rs\n+++ b/src/file.rs\n@@ -4,1 +8,1 @@\n-old\n+new\n")).unwrap();
    }
    if path.ends_with("/pullrequests/7") && method == Method::GET {
        return Json(json!({"id":7,"title":"detail","state":"OPEN","source":{"branch":{"name":"feature/x"}},"destination":{"branch":{"name":"main"}},"participants":[{"approved":true}]})).into_response();
    }
    if path.ends_with("/pullrequests/1") && method == Method::GET {
        return Json(json!({"id":1,"title":"detail","description":"Review context","state":"OPEN","source":{"branch":{"name":"feature/x"}},"destination":{"branch":{"name":"main"}},"participants":[{"approved":true,"user":{"uuid":"{504c3b62-8120-4f0c-a7bc-87800b9d6f70}","account_id":"abc-123","display_name":"Approver"}},{"approved":false,"user":{"uuid":"{reviewer}","display_name":"Reviewer"}}]})).into_response();
    }
    if path.ends_with("/pullrequests") && method == Method::GET {
        let next = if path.contains("/repositories/w/badpage/") {
            "https://evil.invalid/path?page=bad%2Fpage"
        } else {
            "https://evil.invalid/path?page=9"
        };
        return Json(json!({"values":[],"next":next})).into_response();
    }
    if path.ends_with("/pullrequests") && method == Method::POST {
        return match json_body["source"]["branch"]["name"].as_str() {
            Some("feature/unknown") => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ignored":true}))).into_response(),
            Some("feature/rejected") => (StatusCode::BAD_REQUEST, Json(json!({"ignored":true}))).into_response(),
            Some("feature/malformed") => Json(json!({"id":9,"title":"created","state":"OPEN","source":{"branch":{}},"destination":{"branch":{"name":"main"}}})).into_response(),
            _ => Response::builder().status(StatusCode::FOUND).header("location", "/proxy/2.0/outside").body(Body::empty()).unwrap(),
        };
    }
    if path.ends_with("/pullrequests/1/approve") {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .unwrap();
    }
    if path.ends_with("/pullrequests/20/approve") {
        return Json(json!({"approved":true,"secret-as-key":"token-secret"})).into_response();
    }
    if path.ends_with("/pullrequests/21/approve") {
        return Json(json!({"user":{"display_name":"missing"}})).into_response();
    }
    if path.ends_with("/pullrequests/22/approve") {
        return Json(json!({"approved":false})).into_response();
    }
    if path.ends_with("/pullrequests/23/approve") {
        return Response::builder()
            .status(200)
            .body(Body::from("not-json"))
            .unwrap();
    }
    if path.ends_with("/pullrequests/24/approve") {
        return Json(json!({})).into_response();
    }
    if path.ends_with("/comments") && method == Method::POST {
        return Json(json!({"id":42,"content":{"raw":"posted"},"inline":json_body["inline"]}))
            .into_response();
    }
    Json(json!({})).into_response()
}
async fn client() -> (Bitbucket, Seen) {
    let seen = Seen::default();
    let app = Router::new()
        .fallback(any(respond))
        .with_state(seen.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = Config::new(
        "a@b.test",
        "token-secret",
        "w",
        &format!("http://{addr}/proxy/2.0"),
    )
    .unwrap();
    (Bitbucket::new(config).unwrap(), seen)
}
fn target(id: u64) -> PullRequestTarget {
    PullRequestTarget {
        repo_slug: "r".into(),
        pull_request_id: id,
    }
}
#[tokio::test]
async fn safe_diff_redirect_stays_in_configured_prefix_and_auth_never_leaks() {
    let (api, seen) = client().await;
    let diff = api.get_pull_request_diff(target(1)).await.unwrap();
    assert!(!diff.truncated);
    assert!(diff.text.contains("-old"));
    {
        let records = seen.0.lock().unwrap();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|r| r.1.starts_with("/proxy/2.0/")));
        assert!(records[1].3.as_deref().unwrap().starts_with("Basic "));
    }
    assert!(matches!(
        api.get_pull_request_diff(target(2)).await,
        Err(ToolError::UnsafeNavigation)
    ));
    assert_eq!(seen.0.lock().unwrap().len(), 3);
}
#[tokio::test]
async fn oversized_diff_is_marked_truncated_and_cannot_be_used_for_placement() {
    let (api, seen) = client().await;
    let diff = api.get_pull_request_diff(target(3)).await.unwrap();
    assert!(diff.truncated);
    assert!(diff.text.len() <= 500_000);
    let before = seen.0.lock().unwrap().len();
    assert!(
        api.create_pull_request_comment(CreateCommentInput {
            repo_slug: "r".into(),
            pull_request_id: 3,
            comment: CommentInput::Inline {
                body: "review".into(),
                path: "src/file.rs".into(),
                side: DiffSide::New,
                line: 1
            },
        })
        .await
        .is_err()
    );
    assert_eq!(seen.0.lock().unwrap().len(), before + 1);
}
#[tokio::test]
async fn unsupported_diff_formats_remain_readable_but_not_placeable() {
    let (api, _) = client().await;
    for id in [9, 10, 11] {
        let diff = api.get_pull_request_diff(target(id)).await.unwrap();
        assert!(!diff.truncated);
        assert!(!diff.inline_comments_supported);
        assert!(diff.text.starts_with("diff --git"));
    }
}

#[tokio::test]
async fn complete_diff_without_final_newline_is_preserved() {
    let (api, _) = client().await;
    let diff = api.get_pull_request_diff(target(8)).await.unwrap();
    assert!(!diff.truncated);
    assert!(diff.text.ends_with("+last-line"));
}

#[tokio::test]
async fn inline_old_new_lines_are_checked_against_diff_and_payload_uses_from_to() {
    let (api, seen) = client().await;
    for (side, line, key) in [(DiffSide::Old, 4, "from"), (DiffSide::New, 8, "to")] {
        let result = api
            .create_pull_request_comment(CreateCommentInput {
                repo_slug: "r".into(),
                pull_request_id: 1,
                comment: CommentInput::Inline {
                    body: "review".into(),
                    path: "src/file.rs".into(),
                    side,
                    line,
                },
            })
            .await
            .unwrap();
        assert_eq!(result.id, 42);
        let post = seen
            .0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|x| x.0 == "POST")
            .unwrap()
            .2
            .clone();
        assert_eq!(post["inline"][key], line);
    }
    assert!(
        api.create_pull_request_comment(CreateCommentInput {
            repo_slug: "r".into(),
            pull_request_id: 1,
            comment: CommentInput::Inline {
                body: "bad".into(),
                path: "src/file.rs".into(),
                side: DiffSide::Old,
                line: 8
            }
        })
        .await
        .is_err()
    );
}
#[tokio::test]
async fn approvals_exclude_unapproved_reviewer_and_page_token_is_actionable() {
    let (api, seen) = client().await;
    let approvals = api.get_pull_request_approvals(target(1)).await.unwrap();
    assert_eq!(approvals.count, 1);
    assert_eq!(
        approvals.approvers[0].uuid.as_deref(),
        Some("{504c3b62-8120-4f0c-a7bc-87800b9d6f70}")
    );
    assert_eq!(
        approvals.approvers[0].account_id.as_deref(),
        Some("abc-123")
    );
    assert!(matches!(
        api.get_pull_request_approvals(target(7)).await,
        Err(ToolError::Malformed)
    ));
    assert!(matches!(
        api.get_pull_request(target(8)).await,
        Err(ToolError::Malformed)
    ));
    let page = api
        .list_pull_requests(ListPullRequestsInput {
            repo_slug: "r".into(),
            state: None,
            page: None,
            pagelen: None,
        })
        .await
        .unwrap();
    assert_eq!(page.next_page.as_deref(), Some("9"));
    let _ = api
        .list_pull_requests(ListPullRequestsInput {
            repo_slug: "r".into(),
            state: None,
            page: page.next_page,
            pagelen: Some(10),
        })
        .await
        .unwrap();
    assert!(
        seen.0
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.1.contains("?page=9&pagelen=10"))
    );
    assert!(
        !seen
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.1.starts_with("https://evil.invalid"))
    );
    assert!(matches!(
        api.list_pull_requests(ListPullRequestsInput {
            repo_slug: "badpage".into(),
            state: None,
            page: None,
            pagelen: None,
        })
        .await,
        Err(ToolError::Malformed)
    ));
}
#[tokio::test]
async fn sql_removed_lines_and_unicode_bracket_paths_are_placeable() {
    let (api, seen) = client().await;
    let diff = api.get_pull_request_diff(target(4)).await.unwrap();
    assert!(diff.inline_comments_supported);
    let result = api
        .create_pull_request_comment(CreateCommentInput {
            repo_slug: "r".into(),
            pull_request_id: 4,
            comment: CommentInput::Inline {
                body: "review".into(),
                path: "src/日本語/[id].tsx".into(),
                side: DiffSide::Old,
                line: 1,
            },
        })
        .await
        .unwrap();
    assert_eq!(result.id, 42);
    assert!(
        seen.0
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.2["inline"]["from"] == 1)
    );
}

#[tokio::test]
async fn general_and_reply_comments_keep_their_distinct_wire_shapes() {
    let (api, seen) = client().await;
    for (comment, expected) in [
        (
            CommentInput::General {
                body: "general".into(),
            },
            json!({"content":{"raw":"general"}}),
        ),
        (
            CommentInput::Reply {
                body: "reply".into(),
                parent_id: 12,
            },
            json!({"content":{"raw":"reply"},"parent":{"id":12}}),
        ),
    ] {
        let result = api
            .create_pull_request_comment(CreateCommentInput {
                repo_slug: "r".into(),
                pull_request_id: 1,
                comment,
            })
            .await
            .unwrap();
        assert_eq!(result.id, 42);
        let payload = seen
            .0
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|request| request.0 == "POST")
            .unwrap()
            .2
            .clone();
        assert_eq!(payload, expected);
    }
}

#[tokio::test]
async fn mutation_outcomes_are_classified_and_success_values_redact_token() {
    let (api, seen) = client().await;
    let result = api
        .create_pull_request(CreatePullRequestInput {
            repo_slug: "r".into(),
            title: "title".into(),
            source_branch: "feature/a+b/日本語".into(),
            destination_branch: "main".into(),
            description: None,
            reviewers: None,
        })
        .await;
    assert!(matches!(result, Err(ToolError::OutcomeUnknown)));
    assert_eq!(seen.0.lock().unwrap().len(), 1);
    for (branch, expected) in [
        ("feature/unknown", ToolError::OutcomeUnknown),
        ("feature/rejected", ToolError::Rejected(400)),
        ("feature/malformed", ToolError::OutcomeUnknown),
    ] {
        let result = api
            .create_pull_request(CreatePullRequestInput {
                repo_slug: "r".into(),
                title: "title".into(),
                source_branch: branch.into(),
                destination_branch: "main".into(),
                description: None,
                reviewers: None,
            })
            .await;
        assert!(matches!(
            (result, expected),
            (Err(ToolError::OutcomeUnknown), ToolError::OutcomeUnknown)
                | (Err(ToolError::Rejected(400)), ToolError::Rejected(400))
        ));
    }
    let withdrawn = api.withdraw_pull_request_approval(target(1)).await.unwrap();
    assert!(withdrawn.withdrawn);
    assert!(matches!(api.approve_pull_request(target(20)).await, Ok(value) if value.approved));
    for id in [21, 22, 23] {
        assert!(matches!(
            api.approve_pull_request(target(id)).await,
            Err(ToolError::OutcomeUnknown)
        ));
    }
    assert!(matches!(
        api.withdraw_pull_request_approval(target(24)).await,
        Err(ToolError::OutcomeUnknown)
    ));
    let requests = seen.0.lock().unwrap();
    for id in [20, 21, 22, 23] {
        let request = requests
            .iter()
            .find(|r| r.1 == format!("/proxy/2.0/repositories/w/r/pullrequests/{id}/approve?"))
            .unwrap();
        assert_eq!(request.0, "POST");
        assert_eq!(request.2, Value::Null);
        assert_eq!(
            request.3.as_deref(),
            Some("Basic YUBiLnRlc3Q6dG9rZW4tc2VjcmV0")
        );
    }
    let rejected_withdrawal = requests
        .iter()
        .find(|r| r.1 == "/proxy/2.0/repositories/w/r/pullrequests/24/approve?")
        .unwrap();
    assert_eq!(rejected_withdrawal.0, "DELETE");
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.1.contains("/pullrequests/21/approve"))
            .count(),
        1
    );
    let safe = api.sanitize(json!({"value":"token-secret", "token-secret":"dynamic-key"}));
    assert!(!safe.to_string().contains("token-secret"));
}
