# Evidence: a request path a server would rewrite is refused (#681)

| | |
|---|---|
| Base | `main` at `3b2b82e2` (#678) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Found by | an outside model's review of `proxy.rs` and `policy.rs`, confirmed in the code |

## What was wrong

The request policy reads a model request's body, to refuse server-side tools and content by URL, when the path is a model endpoint (`MODEL_ENDPOINTS`, matched on its suffix). On a host that isn't a model vendor, a path that wasn't spelled as an endpoint wasn't read at all. A server decodes or resolves some paths into another before routing them:
- `/v1/chat/completion%73` is `/v1/chat/completions` once decoded;
- `/v1/foo/../chat/completions` resolves to it;
- `/v1/chat/completions/` routes to it on many servers.

So a cell could send a model request to a local server or a gateway whose body the policy never read, and have the server fetch a URL for it.

## What it does now

- **The proxy refuses a path with `%` in it, or a `.` or `..` segment.** No API path Hatchling forwards needs them. A dot inside a name, such as `/v1/a.b/c..d`, is a name and accepted.
- **The policy reads a path with its trailing slashes removed**, so `/v1/chat/completions/` is the model request it routes to.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | GATE |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | GATE |
| the whole workspace's tests | GATE |
| `hatchling-dev audit` | GATE |

New and extended tests:
- `validate_request_refuses_a_path_a_server_would_rewrite`:
  - refused: `/v1/chat/completion%73`, `/v1/chat/%2e%2e/completions`, `/v1/foo/../chat/completions`, `/v1/chat/completions/.`, `/v1/./chat/completions` and `/..`;
  - accepted: `/v1/chat/completions`, `/v1/models/gemma4:12b` and `/v1/a.b/c..d`.
- `a_model_request_is_read_on_any_destination_and_nothing_else_is`, extended: a model request with one or two trailing slashes is still read, and its server-side tool refused, on a gateway, a local server and a vendor.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| `%` accepted in a path | GATE |
| `.` and `..` segments accepted | GATE |
| Trailing slashes left on the path the policy reads | GATE |

**Mutation**, on the lines changed against `3b2b82e2` (`cargo mutants --in-diff`, `hatchling-egress`):

GATE
