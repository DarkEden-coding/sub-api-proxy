# Current deployment

Updated October 23, 2026. power-server is the primary server for every client and
the Mac is the fallback. Each server holds its own provider logins; credentials and
generated configuration stay outside this repository.

## power-server (primary, Linux)

- `cliproxy` runs as the user systemd unit `sap-server` on `100.107.192.39:8318`,
  the Tailscale address, with `node: power-server` and `role: primary`. nginx
  already owns `127.0.0.1:8318` and `:8319` there, so the server binds Tailscale only.
- `management.allow-remote` is true: the Mac's admin panel and `sap` CLI reach it
  through the router. It shares the Mac's client key and management key.
- Accounts were logged in separately (two Claude, one Codex), not copied, so the
  two servers never share a rotating refresh token.
- Lingering is enabled. The old Go `cliproxyapi.service` (127.0.0.1:8317) is
  stopped and disabled; its config and auth files remain in place.
- Its checkout is `~/Custom-Apps/Sub-Api-Proxy`, built with rustup's toolchain.
  Update it there with `packaging/sub-api-proxy/update-local.sh --role server`.

## Mac host (fallback)

- `cliproxy` serves inference, management, and the dashboard on `127.0.0.1:8318`,
  with `node: Darks-MacBook-Air-2` and `role: fallback`.
- `sap-router` listens on `127.0.0.1:8317` and tries power-server, then the local
  server. The admin panel is `http://127.0.0.1:8317/sap/` and therefore shows
  whichever server the router prefers.
- Tailscale Serve forwards tailnet TCP port 8318 to the loopback server. This
  preserves HTTP and WebSocket traffic and requires the server's client key for
  inference. No public Funnel is configured for this port.
- User LaunchAgents run the server, router, menu-bar launcher, and usage collector.
  The collector drains only the Mac's usage queue; power-server has no collector.

Configuration is under `~/.config/sub-api-proxy/`. Account files and SQLite state
are under `~/.local/share/sub-api-proxy/`. Logs, the protected usage JSONL, and
the machine's installation record are under `~/.local/state/sub-api-proxy/`.

## Peer sync

Both `sap.yaml` files list the other node with the shared `peer.secret`. Peer
traffic reaches the Mac through Tailscale Serve and arrives from loopback, so the
Mac sets `allow_loopback_peers: true`; requests remain signed with the secret.

The Mac's op log had already been pruned when power-server joined, and sync has
no snapshot install, so power-server's `cursor` row for the Mac was seeded at the
Mac's `log_floor` (471927). History before that exists only on the Mac. A peer
that stays out of contact past the op-log retention needs the same treatment.

## Remote Linux client (Scythe-Desktop)

A user systemd service runs `sap-router` on `127.0.0.1:8317`, forwarding to
power-server and then the Mac (`100.75.14.59:8318`). Its private `client-key` file
supplies inference authentication. It uses `stable_secs: 30`, a 5000 ms probe
timeout, a 10000 ms connect timeout, and three failures before down. The user
service is enabled at startup and lingering is enabled.

Claude Code uses the local router as `ANTHROPIC_BASE_URL`, enables gateway hint
headers, and retains its native login. Codex uses a custom Responses provider
with WebSockets at `http://127.0.0.1:8317/v1`. Its local provider has no `env_key`
and sets `requires_openai_auth = false`; the router supplies authentication.

## Logging in on power-server

The OAuth forwarder redirects the browser to `127.0.0.1:8318`, which on the Mac is
the Mac's own server. To log in from the Mac, run a local redirector on ports
54545 (Claude) and 1455 (Codex) that sends the browser to
`http://100.107.192.39:8318/{anthropic,codex}/callback` with the same query, then
run `sap login <provider> --router http://100.107.192.39:8318`. Start one Claude
login at a time; they share the callback port.

## Updating

On each server, from its checkout: `packaging/sub-api-proxy/update-local.sh`
(`--role server` on power-server). Update Scythe-Desktop's router from the Mac with
`packaging/sub-api-proxy/update-remote.sh`. The installers preserve configuration
and account files and restart the services. Deployment checks use health, router
status, and the authenticated model list without sending inference requests.
