# sap CLI

`sap` talks to the local router at `http://127.0.0.1:8317`. Use `--router` to
choose another HTTP authority and `--json` for structured output. All management
requests use sap-core API types and Bearer authentication, with key precedence
`--key`, `SAP_MANAGEMENT_KEY`, then `~/.config/sub-api-proxy/management-key`.
The key file must have mode 0600 on Unix. HTTP requests have a five-second timeout
and an eight-MiB response limit. Errors do not echo arbitrary server bodies.

Commands include status, accounts and account controls, active or all threads,
nodes, events, conflicts and resolution, provider login, and manual assignment.
Login prints the browser URL. Thread keys and IDs are percent-encoded as complete
URL path segments.

Setup accepts `--client-key` or `SAP_CLIENT_KEY` for Claude. Codex uses the
`SAP_CLIENT_KEY` environment variable at client launch time. Management keys are
never copied into client settings automatically. Setup backs up before writing,
uses atomic replacement, and supports undo to the latest backup. `--dry-run`
writes nothing and prints no keys. `--home` and setup's `--path` support explicit
isolated paths. See `packaging/sub-api-proxy/README.md` for deployment steps.

## Public Rust API

These entry points support tests and embedding without implicit home-file access:

```rust
pub fn init(path: &Path, server: bool) -> anyhow::Result<()>;

pub enum setup::CodingClient { Claude, Codex }
pub fn setup::settings_path(home: &Path, client: CodingClient) -> PathBuf;
pub fn setup::configure_codex(document: &mut toml_edit::DocumentMut, router: &str)
    -> anyhow::Result<()>;
pub fn setup::setup(path: &Path, client: CodingClient, router: &str,
                    token: Option<&str>, dry_run: bool)
    -> anyhow::Result<Option<PathBuf>>;
pub fn setup::undo(path: &Path, dry_run: bool) -> anyhow::Result<PathBuf>;
pub fn setup::points_at_router(path: &Path, client: CodingClient, router: &str)
    -> anyhow::Result<()>;

pub fn client::management_key(explicit: Option<String>, file: &Path)
    -> anyhow::Result<String>;
pub fn client::segment(value: &str) -> String;
impl client::Client {
    pub fn new(base: &str, key: Option<String>) -> anyhow::Result<Self>;
    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T>;
    pub async fn request<T: DeserializeOwned>(&self, method: http::Method,
        path: &str, body: Option<&impl Serialize>) -> anyhow::Result<T>;
}
pub fn table::render<T: Serialize>(data: &T, columns: &[(&str, &str)]) -> String;
pub fn table::flow(snapshot: &sap_core::api::FlowSnapshot) -> String;
```

`management_key` reads the named environment variable when no explicit key is
provided. Other library file operations use only paths supplied by the caller.
The binary validates router URLs before setup; embedding callers should do the
same via `Client::new`.

## Validation and limits

```sh
CARGO_BUILD_JOBS=4 cargo fmt -p sap-cli
CARGO_BUILD_JOBS=4 cargo clippy -p sap-cli --all-targets -- -D warnings
CARGO_BUILD_JOBS=4 cargo test -p sap-cli
python3 packaging/sub-api-proxy/test-install.py
```

Integration tests cover Claude JSON and Codex TOML preservation, exact backup
restoration, repeated undo and absent files, dry-run, invalid input, init overwrite
refusal, flow fixture rendering, terminal control stripping, and an authenticated
CLI command against a localhost fake server. Installer checks use temporary homes
and fake launchd/systemd commands, including paths with spaces and XML characters.

No sap-core contract changes are requested. Codex custom-provider configuration
remains provisional until phase-0 validation, isolated in `configure_codex`.
HTTPS, automatic browser opening, and provider login completion polling are not
implemented. Doctor validates local config and health, not real provider access
or client protocol parity. Actual service loading and Linux/macOS deployment
require host verification and the project's deployment approval.
