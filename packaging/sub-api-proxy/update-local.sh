#!/bin/sh
# Builds this checkout in release mode and installs it over the running Sub-Api-Proxy
# services with install.sh (binaries, launchd/systemd units, restart). Config files are
# never touched. Run from anywhere; pass --role/--dry-run through to install.sh.
#
#   packaging/sub-api-proxy/update-local.sh              # server and router, as install.sh
#   packaging/sub-api-proxy/update-local.sh --role server
#   packaging/sub-api-proxy/update-local.sh --role router # a client: no dashboards or cliproxy
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
cd "$repo"

role=both
previous=
for arg in "$@"; do
    [ "$previous" != --role ] || role=$arg
    previous=$arg
done

packages='-p sap-cli'
case "$role" in
    router) packages="$packages -p sap-router" ;;
    server) packages="$packages -p cliproxy" ;;
    *) packages="$packages -p cliproxy -p sap-router" ;;
esac

if [ "$role" != router ]; then
    # Both dashboards (ui: management, ui/sap: Sub-Api-Proxy) are embedded in cliproxy,
    # so build them first.
    for dir in ui ui/sap; do
        (cd "$dir" && { [ -d node_modules ] || npm ci; } && npm run build)
    done
fi

echo "Building $(git rev-parse --short HEAD 2>/dev/null || echo 'this checkout') ($role)"
# No pipe here: a failed build must stop the script before install.sh runs.
# shellcheck disable=SC2086
cargo build --release --locked --quiet $packages
exec "$script_dir/install.sh" --bin-dir "$repo/target/release" "$@"
