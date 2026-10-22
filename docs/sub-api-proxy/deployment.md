# Current deployment

Updated October 22, 2026. Provider accounts are managed in the Mac's admin panel.
Credentials and generated configuration stay outside this repository.

## Mac host

- `cliproxy` serves inference, management, and the dashboard on `127.0.0.1:8318`.
- `sap-router` listens on `127.0.0.1:8317` and forwards to that server.
- The admin panel is `http://127.0.0.1:8317/sap/`.
- Tailscale Serve forwards tailnet TCP port 8318 to the loopback server. This
  preserves HTTP and WebSocket traffic and requires the server's client key for
  inference. No public Funnel is configured for this port.
- User LaunchAgents run the server, router, menu-bar launcher, and usage collector.
- The account viewer reads the local router and the private management-key file
  through `~/.config/sub-api-proxy/visualizer.json`.

Configuration is under `~/.config/sub-api-proxy/`. Account files and SQLite state
are under `~/.local/share/sub-api-proxy/`. Logs, the protected usage JSONL, and
the machine's installation record are under `~/.local/state/sub-api-proxy/`.
The installation record identifies the deployed commit and build profile.

## Remote Linux client

A user systemd service runs `sap-router` on `127.0.0.1:8317`, forwarding to the
Mac's Tailscale address on port 8318. Its private `client-key` file supplies
inference authentication. The router records the remote machine's client host
and replaces client credentials before sending requests to the Mac.

The single-server configuration uses `stable_secs: 0`, a 3000 ms probe timeout,
and a 2000 ms connect timeout. There is no alternate backend on this machine.
The user service is enabled at startup and lingering is enabled.

Claude Code uses the local router as `ANTHROPIC_BASE_URL`, enables gateway hint
headers, and retains its native login. Codex uses a custom Responses provider
with WebSockets at `http://127.0.0.1:8317/v1`. Its local provider has no `env_key`
and sets `requires_openai_auth = false`; the router supplies authentication.
Both clients have timestamped configuration backups beside their settings.

The remote tools require the Mac and Tailscale to remain online. The remote SSH
service rejected reverse forwarding, so no SSH tunnel service is used.

## Updating

Build the proxy binaries with `cargo build --release -p cliproxy -p sap-router
-p sap-cli`, then use `packaging/sub-api-proxy/install.sh --role both --bin-dir
target/release` on the Mac. The installer preserves configuration and account
files and restarts the services. The menu-bar app and collector have separate
install scripts documented in the packaging guide.

Rebuild the remote router only when its source changes. Keep its key and router
configuration private. Preserve the existing Tailscale Serve configuration on
other ports when updating port 8318. Deployment checks use health, router status,
and the authenticated model list without sending inference requests. The user
is handling further functional testing.
