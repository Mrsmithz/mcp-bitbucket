# Bitbucket Cloud MCP

A Rust stdio MCP server for Bitbucket Cloud pull-request work. Its Pi companion asks for fresh consent before approving a PR or withdrawing an approval. The extension does not read credentials or make Bitbucket requests.

## Requirements and setup

- Rust toolchain supporting edition 2024 and Rust 1.88 or later.
- Node.js and npm for Pi extension checks (Pi `1.5.0` and `pi-mcp-adapter` `7.0.1`).
- A Bitbucket Cloud API token with only the scopes needed for your work:
  - `read:pullrequest:bitbucket`
  - `write:pullrequest:bitbucket`
  - `read:repository:bitbucket` (needed to read diffs)
- Set `BITBUCKET_EMAIL`, `BITBUCKET_API_TOKEN`, and the required `BITBUCKET_WORKSPACE` in the MCP server's environment. Workspace is fixed for the process and is not an MCP tool argument. `BITBUCKET_BASE_URL` is optional and defaults to `https://api.bitbucket.org/2.0`. It may include a path prefix for a compatible proxy. HTTPS is required except for loopback HTTP used by local tests; URL credentials, query strings, and fragments are rejected. TLS certificate verification is enabled by default. `BITBUCKET_TLS_VERIFY` accepts only `true` or `false`; unset and `true` verify certificates, while `false` explicitly disables verification for every API request and prints a warning to stderr. Use that opt-out only with a trusted test/proxy when its CA cannot be installed; never use it to bypass certificate errors against Bitbucket Cloud.

Use `.env.example` as a reference; the server does not automatically load dotenv files. Supply credentials through your secret manager or MCP host environment. Never commit credentials or put a real token in a shell command. Configure only Bitbucket Cloud or a trusted Cloud-compatible proxy. A custom base URL does not add Server or Data Center support.

With credentials set in the environment, build and run:

```sh
cargo build --locked --release
./target/release/mcp-bitbucket
```

Configure your MCP client to launch the built `mcp-bitbucket` executable over stdio. Example shape (adapt command/environment handling to your host):

```json
{
  "mcpServers": {
    "bitbucket": {
      "command": "/absolute/path/to/mcp-bitbucket",
      "env": {
        "BITBUCKET_EMAIL": "${BITBUCKET_EMAIL}",
        "BITBUCKET_API_TOKEN": "${BITBUCKET_API_TOKEN}",
        "BITBUCKET_WORKSPACE": "${BITBUCKET_WORKSPACE}",
        "BITBUCKET_BASE_URL": "https://api.bitbucket.org/2.0",
        "BITBUCKET_TLS_VERIFY": "true"
      }
    }
  }
}
```

## Tools

The twelve tools are `search_repositories`, `list_pull_requests`, `get_pull_request`, `get_pull_request_diff`, `list_pull_request_files`, `get_pull_request_file_diff`, `list_pull_request_comments`, `create_pull_request`, `create_pull_request_comment`, `approve_pull_request`, `withdraw_pull_request_approval`, and `get_pull_request_approvals`. `search_repositories` lists repositories in the configured workspace and matches a literal, case-insensitive substring in each repository's name or slug. It accepts only `query`, `page`, and `pagelen`; it does not expose a workspace override or upstream query syntax. It fetches one upstream page per call, so an empty `values` array can still include `next_page`. Repository-search cursors are opaque, bound to the query and page size, replayable, and scoped to the running server instance; after a restart or cursor eviction, restart the search.

Reads return bounded data. Pull-request details include the description. Approval results contain only current approved participants with stable identity fields; this is not a merge-readiness check. List tools return `values`, an optional `next_page` token, and `truncated`; pass the token back to the same list tool and repeat any desired filters and page length. Repository-search cursors specifically require the same query and page size. The token is not an authenticated URL. `list_pull_request_files` returns diffstat paths, status, and available line counts, with at most 50 files per page. `get_pull_request_file_diff` accepts one repository-relative path and returns only Bitbucket's server-filtered file patch, capped at 500 KB with an explicit `truncated` flag. Example arguments are `{"repo_slug":"demo","pull_request_id":42,"pagelen":25}` for the list, then `{"repo_slug":"demo","pull_request_id":42,"path":"src/lib.rs"}` for a file. Fetch the file again if the PR changes between listing and reading; Bitbucket may update the comparison between requests. Rename-side filtering follows Bitbucket's upstream behavior and is not guaranteed for both names. These read tools do not change inline-comment placement behavior. Whole-PR diff output is also capped and marks truncation; inline placement is refused when that diff was truncated, its format is unsupported, or the requested side/line is absent. Unsupported formats (including renames and binary diffs) remain readable as bounded raw text but cannot be used for inline placement. Placement is checked against the diff fetched immediately before posting; Bitbucket may update the diff between that check and comment creation, so this is not an atomic guarantee that the comment still refers to the latest diff.

Writes are limited to creating same-repository non-draft pull requests, posting general/reply/single-line inline comments, approving as the token owner, and withdrawing that owner's approval. PR creation requires explicit source and destination branches; reviewer UUIDs are sent as Bitbucket UUID objects. Inline comments require a path and a line from the latest diff. Approval listing reports approved participant identities only; it does not claim merge readiness. A write that loses its response, receives a server error, has a malformed or oversized success response, or encounters a redirect is reported with `isError: true` and structured `error.code: "outcome_unknown"`; known upstream 4xx responses use `upstream_rejected`. Do not retry an unknown write until you have checked Bitbucket.

The HTTP client does not follow redirects automatically and protocol retries are disabled. GET redirects (needed by the Cloud diff endpoint) are followed only when same-origin and within the configured API path prefix, for at most three hops. Mutations are never redirected or automatically retried. Response bodies are bounded while streaming. Upstream error bodies are not returned. Known token and Basic-auth echoes in successful string fields are redacted before tool results.

## Pi consent extension

Run `npm ci` to install this project's dependencies. Configure Pi to load `pi-mcp-adapter` `7.0.1` as its MCP integration; installing npm dependencies alone does not activate the adapter. Pi's built-in MCP integration does not use this approval broker.

In this project, trust the extension in `.pi/extensions/bitbucket-consent.ts`, put the MCP configuration above in `.pi/mcp.json`, and run `/reload`. For a company proxy, replace the example `BITBUCKET_BASE_URL` with its full Cloud-compatible API root. If you copy the extension to another project, make `pi-mcp-adapter` `7.0.1` available to that file's module resolver as well.

The extension listens to the `pi-mcp-adapter:tool-approval-request` broker, not the outer tool-call hook. Each approval or withdrawal requires a fresh **Allow once** decision, even if the adapter already has a session grant. The prompt shows the action, server, repository, and PR ID. The broker request does not identify the server's configured workspace, so the extension does not guess it. Obsolete workspace arguments, invalid IDs/targets, cancellation, UI errors, headless sessions, and session changes fail closed. Reads, PR creation, and comments do not receive this extra prompt.

The extension is tested against pinned Pi `1.5.0` and `pi-mcp-adapter` `7.0.1` dependencies. It asks for fresh consent exactly for `approve_pull_request` and `withdraw_pull_request_approval`; creation and comment tools are not gated by this extension. This is a client-side workflow guard for Pi calls through the `pi-mcp-adapter` broker, not a sandbox or universal server-side human-approval mechanism. It does not constrain shell commands such as `curl`, standalone MCP clients, other clients' controls, or arbitrary local code that can use the same environment token. Direct launches and other clients must enforce their own controls. The Rust server itself makes requests as the configured token owner and cannot infer human consent.

## Verification

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
npm ci --ignore-scripts
npm run typecheck
npm test
python3 scripts/test_smoke_regressions.py
cargo build --locked
python3 scripts/smoke_stdio.py target/debug/mcp-bitbucket
python3 scripts/check_file_diff_contract.py target/debug/mcp-bitbucket
PATH="$PWD/node_modules/.bin:$PATH" python3 scripts/smoke_pi_consent.py
python3 scripts/check_repository_search_contract.py target/debug/mcp-bitbucket
```

The Python smoke scripts require Python 3 and a POSIX environment. The stdio smoke launches the actual Rust binary against a local fake Cloud API and calls all twelve tools, including repository search, each comment kind, and both per-file diff tools. The repository-search contract driver also starts a local self-signed HTTPS fixture with temporary OpenSSL credentials to verify the default TLS rejection and explicit insecure opt-out; it does not contact Bitbucket. Node tests use the real adapter broker and populated permission caches. The Pi smoke drives an isolated, offline Pi `1.5.0` terminal dialog without model requests or account credentials; its direct broker test complements, rather than replaces, the native cache tests.

The file-diff contract check verifies filters on every repository request, pagination context, quoted paths, and streaming limits. Its unfiltered PR response body is gated, so the check fails if the client reads that body before rejecting the response.

The commands above use local fixtures, not a live Bitbucket account. The per-file tools also passed a controlled read-only Cloud check for changed-file listing, a selected patch, and an empty result for a nonexistent path.
