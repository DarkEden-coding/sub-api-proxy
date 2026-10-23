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
