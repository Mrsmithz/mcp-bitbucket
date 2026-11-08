use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::{Child, ChildStdout, Command},
    time::timeout,
};

type RequestRecord = (String, String, Value, Option<String>);
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<RequestRecord>>>);
async fn endpoint(
    State(seen): State<Seen>,
    uri: Uri,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    seen.0
        .lock()
        .unwrap()
        .push((method.to_string(), path.clone(), value.clone(), auth));
    if method == Method::GET && path.ends_with("/pullrequests") {
        return Json(json!({"values":[]})).into_response();
    }
    if method == Method::GET && path.ends_with("/comments") {
        return Json(json!({"values":[]})).into_response();
    }
    if method == Method::GET && path.ends_with("/diff") {
        return (
            StatusCode::OK,
            "diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n",
        )
            .into_response();
    }
    if method == Method::POST && path.ends_with("/approve") {
        return Json(
            json!({"approved":true,"user":{"uuid":"{504c3b62-8120-4f0c-a7bc-87800b9d6f70}"}}),
        )
        .into_response();
    }
    if method == Method::DELETE && path.ends_with("/approve") {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(axum::body::Body::empty())
            .unwrap();
    }
    if method == Method::POST && path.ends_with("/comments") {
        return Json(json!({"id":8,"content":{"raw":"posted"}})).into_response();
    }
    if path.ends_with("/pullrequests/404") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"private upstream detail"})),
        )
            .into_response();
    }
    let result = if method == Method::POST {
        json!({"id":9,"title":"created","state":"OPEN","source":{"branch":{"name":"feature/x"}},"destination":{"branch":{"name":"main"}}})
    } else {
        json!({"id":1,"title":"secret-token","description":"review context","state":"OPEN","source":{"branch":{"name":"feature/x"}},"destination":{"branch":{"name":"main"}},"author":{"display_name":"user"},"participants":[{"approved":true,"user":{"uuid":"{504c3b62-8120-4f0c-a7bc-87800b9d6f70}","account_id":"abc-123","display_name":"Approver"}}]})
    };
    Json(result).into_response()
}
async fn start() -> (String, Seen) {
    let seen = Seen::default();
    let app = Router::new()
        .fallback(any(endpoint))
        .with_state(seen.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/proxy/2.0"), seen)
}
struct Harness {
    child: Child,
    stdout: BufReader<ChildStdout>,
}
impl Harness {
    async fn launch(base: String) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcp-bitbucket"))
            .env("BITBUCKET_EMAIL", "test@example.com")
            .env("BITBUCKET_API_TOKEN", "secret-token")
            .env("BITBUCKET_WORKSPACE", "acme")
            .env("BITBUCKET_BASE_URL", base)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdout }
    }
    async fn send(&mut self, message: Value) {
        let input = self.child.stdin.as_mut().unwrap();
        input
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
        input.flush().await.unwrap();
    }
    async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await;
    }
    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await;
        loop {
            let mut line = String::new();
            timeout(Duration::from_secs(3), self.stdout.read_line(&mut line))
                .await
                .expect("stdio response timed out")
                .expect("read stdio response");
            assert!(!line.is_empty(), "stdio server exited before replying");
            let response: Value = serde_json::from_str(&line).expect("valid JSON-RPC response");
            if response["id"] == id {
                return response;
            }
        }
    }
}
async fn launch_without_workspace(base: &str, workspace: Option<&str>) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mcp-bitbucket"));
    command
        .env_clear()
        .env("BITBUCKET_EMAIL", "test@example.com")
        .env("BITBUCKET_API_TOKEN", "secret-token")
        .env("BITBUCKET_BASE_URL", base)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(workspace) = workspace {
        command.env("BITBUCKET_WORKSPACE", workspace);
    }
    command.output().await.unwrap()
}

#[tokio::test]
async fn required_workspace_validation_fails_before_stdio_or_network() {
    let (base, seen) = start().await;
    for workspace in [None, Some(""), Some("   "), Some("../unsafe")] {
        let output = launch_without_workspace(&base, workspace).await;
        assert!(
            !output.status.success(),
            "workspace {workspace:?} unexpectedly started"
        );
        assert!(
            output.stdout.is_empty(),
            "invalid config served stdio requests"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("BITBUCKET_WORKSPACE"));
        if let Some(workspace) = workspace.filter(|value| !value.is_empty()) {
            assert!(
                !stderr.contains(workspace),
                "workspace value leaked in startup error"
            );
        }
    }
    assert!(
        seen.0.lock().unwrap().is_empty(),
        "invalid config reached HTTP"
    );
}

#[tokio::test]
async fn every_schema_and_route_uses_only_the_configured_workspace() {
    let (base, seen) = start().await;
    let mut harness = Harness::launch(base).await;
    let initialized = harness.call(1, "initialize", json!({
        "protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}
    })).await;
    assert_eq!(initialized["id"], 1);
    harness.notify("notifications/initialized", json!({})).await;
    let tools = harness.call(2, "tools/list", json!({})).await["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    let names = [
        "list_pull_requests",
        "get_pull_request",
        "get_pull_request_diff",
        "list_pull_request_comments",
        "create_pull_request",
        "create_pull_request_comment",
        "approve_pull_request",
        "withdraw_pull_request_approval",
        "get_pull_request_approvals",
    ];
    assert_eq!(tools.len(), names.len());
    for name in names {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
        assert!(
            tool["inputSchema"]["properties"].get("workspace").is_none(),
            "{name} exposes workspace"
        );
    }
    let arguments = [
        ("list_pull_requests", json!({"repo_slug":"repo"})),
        (
            "get_pull_request",
            json!({"repo_slug":"repo","pull_request_id":1}),
        ),
        (
            "get_pull_request_diff",
            json!({"repo_slug":"repo","pull_request_id":1}),
        ),
        (
            "list_pull_request_comments",
            json!({"repo_slug":"repo","pull_request_id":1}),
        ),
        (
            "create_pull_request",
            json!({"repo_slug":"repo","title":"title","source_branch":"feature","destination_branch":"main"}),
        ),
        (
            "create_pull_request_comment",
            json!({"repo_slug":"repo","pull_request_id":1,"comment":{"kind":"general","body":"comment"}}),
        ),
        (
            "approve_pull_request",
            json!({"repo_slug":"repo","pull_request_id":1}),
        ),
        (
            "withdraw_pull_request_approval",
            json!({"repo_slug":"repo","pull_request_id":1}),
        ),
        (
            "get_pull_request_approvals",
            json!({"repo_slug":"repo","pull_request_id":1}),
        ),
    ];
    for (index, (name, args)) in arguments.iter().enumerate() {
        let mut legacy = args.clone();
        legacy["workspace"] = json!("attacker-workspace");
        let response = harness
            .call(
                10 + index as u64,
                "tools/call",
                json!({"name":name,"arguments":legacy}),
            )
            .await;
        assert!(
            response.get("error").is_some() || response["result"]["isError"] == true,
            "legacy workspace argument was accepted by {name}: {response}"
        );
    }
    assert!(
        seen.0.lock().unwrap().is_empty(),
        "legacy input reached HTTP"
    );
    for (index, (name, args)) in arguments.iter().enumerate() {
        let response = harness
            .call(
                30 + index as u64,
                "tools/call",
                json!({"name":name,"arguments":args}),
            )
            .await;
        assert!(
            response.get("error").is_none(),
            "valid {name} call failed protocol: {response}"
        );
        assert_ne!(
            response["result"]["isError"], true,
            "valid {name} call failed: {response}"
        );
    }
    {
        let records = seen.0.lock().unwrap();
        assert_eq!(
            records.len(),
            names.len(),
            "each valid tool must make one request"
        );
        assert!(
            records
                .iter()
                .all(|(_, path, _, _)| path.starts_with("/proxy/2.0/repositories/acme/repo/")),
            "a request did not use the configured workspace: {records:?}"
        );
    }
    assert!(harness.child.kill().await.is_ok());
}

#[tokio::test]
async fn spawned_stdio_handshake_and_calls_are_id_correlated_and_semantically_checked() {
    let (base, seen) = start().await;
    let mut harness = Harness::launch(base).await;
    let initialized = harness.call(71, "initialize", json!({
        "protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1"}
    })).await;
    assert_eq!(initialized["id"], 71);
    assert!(
        initialized["result"]["serverInfo"]["name"]
            .as_str()
            .is_some()
    );
    harness.notify("notifications/initialized", json!({})).await;

    let listed = harness.call(72, "tools/list", json!({})).await;
    assert_eq!(listed["id"], 72);
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for name in [
        "list_pull_requests",
        "get_pull_request",
        "get_pull_request_diff",
        "list_pull_request_comments",
        "create_pull_request",
        "create_pull_request_comment",
        "approve_pull_request",
        "withdraw_pull_request_approval",
        "get_pull_request_approvals",
    ] {
        assert!(names.contains(&name), "missing {name}");
    }

    let detail = harness
        .call(
            73,
            "tools/call",
            json!({"name":"get_pull_request","arguments":{"repo_slug":"repo","pull_request_id":1}}),
        )
        .await;
    assert_eq!(detail["id"], 73);
    assert_ne!(detail["result"]["isError"], true);
    assert_eq!(detail["result"]["structuredContent"]["id"], 1);
    assert_eq!(
        detail["result"]["structuredContent"]["description"],
        "review context"
    );
    assert!(
        !detail["result"]["structuredContent"]
            .to_string()
            .contains("secret-token")
    );

    let approvals = harness.call(74, "tools/call", json!({"name":"get_pull_request_approvals","arguments":{"repo_slug":"repo","pull_request_id":1}})).await;
    assert_ne!(approvals["result"]["isError"], true);
    assert_eq!(approvals["result"]["structuredContent"]["count"], 1);
    assert_eq!(
        approvals["result"]["structuredContent"]["approvers"][0]["account_id"],
        "abc-123"
    );

    let invalid = harness
        .call(
            75,
            "tools/call",
            json!({"name":"get_pull_request","arguments":{"repo_slug":"repo","pull_request_id":0}}),
        )
        .await;
    assert_eq!(invalid["id"], 75);
    assert_eq!(invalid["result"]["isError"], true);
    assert!(
        invalid["result"]["structuredContent"]["error"]["code"]
            .as_str()
            .is_some()
    );

    let upstream_error = harness.call(76, "tools/call", json!({"name":"get_pull_request","arguments":{"repo_slug":"repo","pull_request_id":404}})).await;
    assert_eq!(upstream_error["result"]["isError"], true);
    assert_eq!(
        upstream_error["result"]["structuredContent"]["error"]["http_status"],
        404
    );
    assert!(
        !upstream_error["result"]["structuredContent"]
            .to_string()
            .contains("private upstream detail")
    );

    let created = harness.call(77, "tools/call", json!({"name":"create_pull_request","arguments":{"repo_slug":"repo","title":"Title","source_branch":"feature/x","destination_branch":"main","reviewers":["504c3b62-8120-4f0c-a7bc-87800b9d6f70"]}})).await;
    assert_ne!(created["result"]["isError"], true);
    assert_eq!(created["result"]["structuredContent"]["id"], 9);
    {
        let posted = seen.0.lock().unwrap();
        let (_, path, payload, auth) = posted.iter().find(|(m, _, _, _)| m == "POST").unwrap();
        assert!(path.starts_with("/proxy/2.0/repositories/acme/repo/pullrequests"));
        assert_eq!(
            payload["reviewers"][0]["uuid"],
            "{504c3b62-8120-4f0c-a7bc-87800b9d6f70}"
        );
        assert_eq!(payload["source"]["branch"]["name"], "feature/x");
        assert!(auth.as_deref().unwrap().starts_with("Basic "));
    }
    assert!(harness.child.kill().await.is_ok());
}
