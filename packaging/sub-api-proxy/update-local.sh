#!/bin/sh
# Builds this checkout in release mode and installs it over the running Sub-Api-Proxy
# services with install.sh (binaries, launchd/systemd units, restart). Config files are
# never touched. Run from anywhere; pass --role/--dry-run through to install.sh.
#
#   packaging/sub-api-proxy/update-local.sh              # server and router, as install.sh
#   packaging/sub-api-proxy/update-local.sh --role server
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
cd "$repo"

# Both dashboards (ui: management, ui/sap: Sub-Api-Proxy) are embedded in the binaries,
# so build them first.
for dir in ui ui/sap; do
    (cd "$dir" && { [ -d node_modules ] || npm ci; } && npm run build)
done

cargo build --release -p cliproxy -p sap-router -p sap-cli 2>&1 | tail -5
exec "$script_dir/install.sh" --bin-dir "$repo/target/release" "$@"
