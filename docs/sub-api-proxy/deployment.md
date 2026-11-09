# Current deployment

Updated November 7, 2026. power-server is the primary server for every client and
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

The automatic callback helper is deployed on the Mac and Scythe-Desktop. Both the
Mac fallback proxy and power-server have the updated dashboard and login API.
Existing launchd and systemd services run the new binaries persistently. Protected
rollback copies are under `~/.local/state/sub-api-proxy/deployments/oauth-*` on
each machine. Other browser devices need the updated router before signing in.

Open either the power server's Tailscale dashboard directly or
the local router's dashboard and use Add account or Sign in again. The dashboard
hands the session to the browser device's local router before opening Claude or
Codex. That helper returns the callback to the server that started the login,
even if another server is preferred. No temporary redirector is needed.

The helper's `router.yaml` must list every account server. Add any MagicDNS,
Tailscale Serve or other dashboard address as an `oauth_origins` alias on its
issuing server. See [automatic account sign-in](../../crates/sap-router/README.md#automatic-account-sign-in)
for configuration, startup services and validation. Callback ports 54545 and 1455
must be free on the browser device.

## Updating

On each server, from its checkout: `packaging/sub-api-proxy/update-local.sh`.
Use the default role on power-server too: it also runs `sap-router`, and
`--role server` leaves that router binary unchanged. Update Scythe-Desktop's router from the Mac with
`packaging/sub-api-proxy/update-remote.sh`. The installers preserve configuration
and account files and restart the services. Deployment checks use health, router
status, and the authenticated model list without sending inference requests.

## November 7 OAuth deployment checks

Both proxies report serving, deliver the tested dashboard asset
`index-Cj3T1aQJ.js`, and return their authenticated model lists. The Mac and Scythe
helpers answer HTTP 200 and prefer power-server. Claude and Codex callback-helper
session creation and cancellation passed on both deployed proxies without
contacting either provider. A complete provider login still requires approval
in the user's browser.

Usage statistics and Claude cache telemetry remain explicitly enabled on both
proxies. The Mac collector file is protected with mode 0600 and retains schema v2
records with ordered timestamps and TTL buckets. Its latest observed record
predates this restart; post-deployment checks on natural traffic remain pending.
No synthetic inference requests were sent. Neither server's provider credentials
were copied or changed during deployment.
