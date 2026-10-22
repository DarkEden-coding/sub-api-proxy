# Sub-Api-Proxy deployment

These units run as the logged-in user. The installer writes only under `$HOME`,
requires no sudo, and leaves existing configuration alone. Running it again
replaces binaries and units and restarts the selected services. It does not build
binaries, provision credentials, or change firewall rules.

Replacing Core's running proxy or publishing a release requires Vayun's explicit
approval. Prepare and review the files before scheduling that cutover.

## Prepare binaries and keys

Build `sap`, `sap-router`, and `cliproxy` for the target host using the repository's
build workflow. Put the executables in a directory passed as `--bin-dir`. This
CLI workstream's validation builds only `sap-cli`.

Create `~/.config/sub-api-proxy` and `~/.local/share/sub-api-proxy`, mode 0700.
Use separate random client and management keys. Store the management key in
`~/.config/sub-api-proxy/management-key`, mode 0600. The CLI also accepts `--key`
or `SAP_MANAGEMENT_KEY`; the file avoids a key in shell history or process args.
Store the client key in `~/.config/sub-api-proxy/client-key`, mode 0600, matching
the router's `client_key_file` and both servers' client access keys.

The server's `config.yaml` uses cliproxy's existing configuration format. Start
from the project's documented config, retain the intended providers, and set:

```yaml
host: "100.64.0.1" # this host's Tailscale address
port: 8318
auth-dir: "/home/YOU/.cli-proxy-api"
api-keys:
  - "YOUR-CLIENT-KEY"
remote-management:
  allow-remote: true
  secret-key: "YOUR-MANAGEMENT-KEY"
```

Substitute actual values and paths. Keep this file mode 0600. The management key
must match the CLI's management-key file. Use the same client access configuration
on both servers. Credentials remain local and must be logged in independently on
each server. Management and inference must be reachable only on the private
network. Never expose these HTTP listeners to the public internet.

## Power-server on Linux

1. Generate the server config with `sap init server --path ~/.config/sub-api-proxy/sap.yaml`.
   Set `node: power-server`, `role: primary`, and an absolute `store_path`, such as
   `/home/YOU/.local/share/sub-api-proxy/state.db`. Ensure its parent exists.
2. Create `config.yaml` as above, listening on power-server's Tailscale IP at
   port 8318. Configure provider logins using the existing cliproxy login workflow.
3. Add the Mac peer to `sap.yaml` when ready:

   ```yaml
   peers:
     - id: macbook
       url: http://100.64.0.2:8318
       secret_file: peer.secret
   ```

   Create `peer.secret` next to `sap.yaml`, mode 0600. Both nodes must use the same
   exact secret bytes. Node IDs must match the peer's `node`. See
   `docs/sub-api-proxy/sap-server-node.md` for the full schema and sync restrictions.
4. Preview installation, then install after deployment approval:

   ```sh
   ./packaging/sub-api-proxy/install.sh --role server --bin-dir /path/to/binaries --dry-run
   ./packaging/sub-api-proxy/install.sh --role server --bin-dir /path/to/binaries
   systemctl --user status sap-server
   journalctl --user -u sap-server
   ```

A user systemd manager is required. For startup without login, an administrator
can enable lingering for this user with `sudo loginctl enable-linger "$USER"`.
The installer never runs sudo or changes that policy itself.

## Mac fallback and client

1. Create `config.yaml` with the Mac's Tailscale IP at port 8318 and its own
   absolute auth-dir. Run `sap init server --path ~/.config/sub-api-proxy/sap.yaml`.
   Set `node: macbook`, `role: fallback`, and an absolute store path under the
   Mac user's `~/.local/share/sub-api-proxy`. Add power-server as a peer at its
   Tailscale IP and port 8318. Install the shared peer secret, mode 0600, and log
   in each provider locally.
2. Run `sap init router --path ~/.config/sub-api-proxy/router.yaml`. Replace the
   example localhost server addresses with power-server's and the Mac's Tailscale
   addresses at port 8318, in that order. Set `client_host` to the Mac's name.
   Keep `listen: 127.0.0.1:8317`. Server and router must use different ports.
3. With Python 3 installed and in a logged-in macOS GUI session, preview and
   install the LaunchAgents after deployment approval:

   ```sh
   ./packaging/sub-api-proxy/install.sh --role both --bin-dir /path/to/binaries --dry-run
   ./packaging/sub-api-proxy/install.sh --role both --bin-dir /path/to/binaries
   launchctl print "gui/$(id -u)/io.sub-api-proxy.router"
   launchctl print "gui/$(id -u)/io.sub-api-proxy.server"
   ```

   Logs are in `~/.local/state/sub-api-proxy/{router,server}.log`. LaunchAgents
   run during the user's login session, so this fallback requires that session.
   To add the menu-bar launcher, build and install the native app:

   ```sh
   sh packaging/sub-api-proxy/macos/build.sh
   sh packaging/sub-api-proxy/macos/install.sh
   ```

   Click its branching-arrow icon to open the admin panel. Right-click and choose
   Copy admin key, then paste it into the panel's Settings page and save. The app
   starts at login. Quitting it leaves the router and server running.
   For continuous cache telemetry collection on the Mac, enable both usage
   statistics and `observability.usage.claude-cache-telemetry` in `config.yaml`,
   then run `sh packaging/sub-api-proxy/install-collector.sh`. The collector
   drains the local usage queue to `~/.local/state/sub-api-proxy/usage.jsonl`,
   mode 0600, keeping only the allowlisted telemetry fields. Use one queue
   collector per server. This file grows until archived or rotated manually.
4. Put the management key in the mode-0600 file described above. Set
   `SAP_CLIENT_KEY` in the environment from which you launch your coding client.
   This is a client key accepted by both servers, not the management key.

   ```sh
   sap setup claude --dry-run
   sap setup claude
   sap setup codex --dry-run
   sap setup codex
   sap doctor
   sap status
   sap accounts
   sap login claude
   sap login codex
   ```

   Login prints a URL to open manually. Complete each server's provider login
   separately using that server's management endpoint when needed.

Claude setup sets `env.ANTHROPIC_BASE_URL` to the router and
`env.CLAUDE_CODE_GATEWAY_HINT_HEADERS=1`, and removes `ANTHROPIC_AUTH_TOKEN` and
`ANTHROPIC_API_KEY`. Claude Code keeps its own subscription login, which keeps the
subscription betas (including the 1-hour prompt cache). The router replaces that
login with the client key from `client_key_file` on inference paths, so the token
never leaves the machine. Codex setup selects `sub-api-proxy` and adds the provider
with `/v1`, `wire_api = "responses"`, `supports_websockets = true` and
`env_key = "SAP_CLIENT_KEY"`; export `SAP_CLIENT_KEY` with the same client key.
See `docs/sub-api-proxy/client-config.md` for why.
Existing TOML comments and unrelated values remain. Backups sit next to the
settings file and have mode 0600. Setup makes a timestamped backup before every
write; undo restores and consumes the latest backup, or removes a file that did
not exist before setup. Dry-run validates without writing or printing keys.

```sh
sap setup --undo claude
sap setup --undo codex
```

Use `--json` on any command for machine-readable output. `--router` overrides the
local endpoint. `--home` and setup's `--path` allow isolated configuration paths.
`doctor --config PATH` checks that router config, local status, each configured
server's health, both client settings, and a management-authenticated API call.
It returns a nonzero exit status when any check fails. It sends health requests
to configured servers only and never makes provider requests.

On another Linux client, configure only `router.yaml` and use `--role router`.
Stop services with `systemctl --user disable --now sap-router sap-server` on Linux
or `launchctl bootout gui/$(id -u)/io.sub-api-proxy.router` and the corresponding
server label on macOS. Remove their user unit files to uninstall; keep config,
backups, and credential directories until you have verified recovery.

## Verification

```sh
CARGO_BUILD_JOBS=4 cargo fmt -p sap-cli
CARGO_BUILD_JOBS=4 cargo clippy -p sap-cli --all-targets -- -D warnings
CARGO_BUILD_JOBS=4 cargo test -p sap-cli
python3 packaging/sub-api-proxy/test-install.py
```

Tests use explicit temporary paths and a localhost fake HTTP server. No test
loads a real auth-dir, changes real client settings, starts services, or calls a
provider. An actual failover drill and client feature checks still need the mock
client harness and an approved deployment.
