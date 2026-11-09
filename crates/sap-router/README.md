# sap-router

A client-local HTTP/1.1 router. It streams request and response bodies, and tunnels
WebSocket wire frames unchanged. It does not parse inference bodies or log traffic;
it logs server state changes and failed requests (method and path, never the query
or headers) to stderr, which the launchd and systemd units send to the router log.
`RUST_LOG` overrides the default `info` level.
`Authorization` passes through, `Host` identifies the selected server, and
`x-sap-client-host` is overwritten with the configured machine name. Hop-by-hop
headers are removed and the required WebSocket upgrade headers are restored.

## Configuration and CLI

Run `sap-router --config /path/to/router.yaml`. Without `--config`, the path is
`~/.config/sub-api-proxy/router.yaml`. `--help` and `--version` are also available.
Configuration is loaded once at startup. Ctrl-C stops probes and connections.

```yaml
listen: 127.0.0.1:8317
client_host: workstation
servers:
  - name: primary
    base_url: http://127.0.0.1:8318
  - name: backup
    base_url: http://127.0.0.1:8319
probe_interval_ms: 1000
probe_timeout_ms: 1500
down_after_failures: 2
stable_secs: 30
connect_timeout_ms: 1000
```

All scalar settings above show defaults except `client_host`, which defaults to
`hostname`. `servers` is required, ordered, and must have unique nonempty names.
URLs must be HTTP authorities with no credentials, path prefix, or query. Unknown
configuration fields, invalid header values, and zero probe/connect durations or
failure thresholds are rejected.

`GET /sap/router/status` is always local and has no authentication:

```json
{"preferred":"primary","servers":[{"name":"primary","state":"healthy","consecutive_failures":0,"stable":true}]}
```

`preferred` is the first stable server, or null when none is stable. States are
`healthy`, `suspect`, and `down`. A recovered server reports `healthy` with
`stable: false` until its stability period ends. Initial successful probes need no
stability delay.

## Failover

Only a successful HTTP response decoding as `sap_core::health::HealthReport` with
`NodeState::Serving` counts as health. Probes start immediately and run on the
configured interval, overlapping when timeout exceeds interval. Responses are
limited to 64 KiB; obsolete probe results cannot overwrite newer ones.

One failure makes a server suspect and moves it behind every stable server. The
configured consecutive failure threshold marks it down. Recovery requires
continuous successful probes for `stable_secs` before it is preferred again.
Existing streams stay with their server during failback.

When no server is stable, requests still try the others: recovering servers
first, then suspect, then down, each in configured order. A server that cannot
accept the connection costs at most `connect_timeout_ms` before the next one is
tried. With a single server, a slow probe or a laptop waking from sleep therefore
no longer refuses every request for the stability period.

Each request establishes its server connection before consuming its body.
Connection refusal or timeout tries the next server without replaying bytes.
Once transmission starts, an error before response headers, or the server going
down while another server is stable, produces HTTP 503 with this body:

```json
{"type":"error","error":{"type":"overloaded_error","message":"Server unavailable; retry through the local router"}}
```

A Down transition aborts an open HTTP stream or closes a WebSocket transport
only when another server is stable to take the retry. Without one, the transfer
stays on its server and ends only if that connection itself fails.
The router never replays a transmitted request. Healthy inference connections
have no silence timeout. WebSockets use bounded bidirectional copy buffers and
preserve masking, fragmentation, control frames, and negotiated extensions.

## Public Rust entry points

```rust
Config::load(path: Option<PathBuf>) -> anyhow::Result<Config>
Config::validate(&self) -> anyhow::Result<()>
serve(config: Config, listener: TcpListener,
      shutdown: impl Future<Output = ()> + Send + 'static) -> anyhow::Result<()>
```

`config::{Config, Server, Cli}` are public. `Config` implements `Default` and
`Deserialize`; `Cli` implements clap's `Parser`. Health and transport types are
private. Cross-crate health data and the client-host header use sap-core directly.

## Validation and limits

Run each command with `CARGO_BUILD_JOBS=4` and an external time limit, such as
180 seconds:

```sh
cargo fmt -p sap-router
cargo clippy -p sap-router --all-targets -- -D warnings
cargo test -p sap-router
```

Eight tests cover configuration, exact binary bodies and raw paths, duplex request
streaming, healthy silence, killed/refused servers, blackholes, ambiguous requests,
continuous recovery, retained backup streams on failback, a single server that
keeps serving through probe failures, and WebSocket text/binary/control traffic
and forced closure. Servers bind only localhost and
never load credentials or contact providers. Every asynchronous scenario has an
outer timeout. `cargo test -p sap-router -- --nocapture` prints failover measurements.

Measured on macOS in this worktree, with in-process localhost servers:

| Scenario | Settings | Elapsed |
| --- | --- | --- |
| Connection refusal to successful backup response | 50 ms interval, 80 ms probe timeout | 1.55 ms |
| Kill to preferred backup | Same | 2.07 ms |
| Blackhole to pending-request 503, stream termination and successful backup body | Default probes: 1000 ms interval, 1500 ms timeout, two failures | 3.49 s |
| WebSocket closure on Down | 50 ms interval, 80 ms probe timeout | 171 ms |

These are single test-run measurements, not production latency guarantees. With
default overlapping probes, a failure just after a successful probe can take
about 3.5 seconds to reach two timeouts. A first failed probe can send new
requests to backup sooner. The kill/refusal test exercises the connection-error
path immediately rather than waiting for two probes.

Known gaps: HTTPS/TLS, HTTP/2, connection pooling, dynamic config/sync updates,
launchd/systemd units, and Linux validation are not implemented here. The tests
exercise client transport closure; actual Codex/Claude reconnection and full-history
resend remain to be validated with their mock-provider harnesses. No real provider
or deployment was used. No sap-core contract changes are requested.

## Automatic account sign-in

Run the updated `sap-router` on **each device where you use the browser**. The
router is also the callback helper. The dashboard can be opened directly on any
configured server over Tailscale, or through the local router.

The dashboard opens the local helper before starting a login. The helper reserves
Claude's registered `localhost:54545/callback` or Codex's registered
`localhost:1455/auth/callback`, then opens the provider. It forwards the returned
query unchanged to the issuing server's `/anthropic/callback` or `/codex/callback`
over the configured transport, normally Tailscale. It never substitutes a
Tailscale address into the provider's OAuth redirect URI. The token exchange and
PKCE verifier stay on the issuing server.

Dashboard logins send `callback_relay: true` to `/sap/api/accounts/login`. The
server skips its own callback listener for those sessions. CLI and ordinary
Management API logins keep their existing behavior. Update both the dashboard's
proxy binary and the device's helper; an old proxy is rejected before opening
the provider.

The helper accepts only servers in its `router.yaml`. If you open a dashboard by
a different address, add that exact origin under the corresponding server:

```yaml
listen: 127.0.0.1:8317
servers:
  - name: power-server
    base_url: http://100.107.192.39:8318
    oauth_origins:
      - http://power-server:8318
      - https://power-server.example.ts.net
  - name: local-fallback
    base_url: http://127.0.0.1:8318
    oauth_origins:
      - http://100.75.14.59:8318
```

`base_url` is automatically accepted as a dashboard origin. `oauth_origins` are
optional browser-facing aliases, including HTTPS reverse proxies; the callback
still goes to `base_url`. Each origin must identify exactly one server. Never
map an origin to a different account server or to a router that can select other
servers. Restart the helper after editing its configuration.

The Accounts page's **Local sign-in helper** field defaults to
`http://127.0.0.1:8317`. Change it if this device's router uses a different port;
the value is saved in that browser. Only HTTP loopback addresses are accepted.
The handoff uses a top-level helper window and origin-checked messages, avoiding
cross-origin fetches to localhost from an HTTPS dashboard. Allow the dashboard's
popup. If the helper is missing, no provider login is started and the dashboard
shows an error.

Install or update the persistent helper on macOS or Linux with
`packaging/sub-api-proxy/update-local.sh --role router` from the updated checkout.
This installs its launchd or systemd user service. The proxy hosting the dashboard
also needs the updated embedded assets, built by the server/both update roles.

Callback states are isolated by provider and pinned to the issuing server,
including during failover. They expire after five minutes and are consumed once.
The listener closes after its last callback or expiry. Ports 54545 and 1455 must
be free on the browser's device; the helper reports a conflict before opening the
provider. Stop another native login using the same port and start again.
Management keys, provider tokens and PKCE verifiers are never sent to the helper
page. Registration is restricted to loopback peers with a loopback Host header
and a same-origin request from the helper page.

Validation: `cargo test -p sap-router`, the dashboard's `npm test` and `npm run
check`, plus `CHROME=/path/to/chromium node ui/sap/scripts/oauth-check.mjs` after
building the dashboard and `cargo build -p sap-router`. The browser check uses two
private local mock servers and intercepted provider pages. It needs free callback
ports, never reads an auth directory, and never contacts a real provider. It checks
both providers, sign-in again, issuer pinning while another server is preferred,
and helper error handling. A real provider sign-in still needs a user to approve
OAuth in their browser.
