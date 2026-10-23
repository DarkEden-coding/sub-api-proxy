#!/bin/sh
# Updates a router-only client over SSH: fast-forwards its git checkout to origin, then
# runs update-local.sh --role router there (build, install, restart). Push first: the
# client builds what is on origin, not this checkout. Config files are never touched.
#
#   packaging/sub-api-proxy/update-remote.sh                  # client from install.json
#   packaging/sub-api-proxy/update-remote.sh dark@host        # a specific client
#   packaging/sub-api-proxy/update-remote.sh dark@host --dry-run
#
# SAP_REMOTE_DIR overrides the client's checkout (default ~/Custom-Apps/Sub-Api-Proxy);
# SAP_REMOTE_BRANCH overrides the branch it follows (default main).
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH='' cd -- "$script_dir/../.." && pwd)
install_json=$HOME/.local/state/sub-api-proxy/install.json

host=
case "${1:-}" in
    -*|'') ;;
    *) host=$1; shift ;;
esac
if [ -z "$host" ] && [ -f "$install_json" ]; then
    host=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1])).get("remote_client") or "")' "$install_json")
fi
[ -n "$host" ] || { echo "Usage: $0 [user@host] [--dry-run]  (no remote_client in $install_json)" >&2; exit 2; }
dir=${SAP_REMOTE_DIR:-'~/Custom-Apps/Sub-Api-Proxy'}
branch=${SAP_REMOTE_BRANCH:-main}

# The client builds origin; say so when this checkout has something else.
git -C "$repo" fetch --quiet origin "$branch"
local_head=$(git -C "$repo" rev-parse HEAD)
origin_head=$(git -C "$repo" rev-parse "origin/$branch")
if [ "$local_head" != "$origin_head" ]; then
    echo "Note: local HEAD $(git -C "$repo" rev-parse --short HEAD) differs from origin/$branch" \
        "$(git -C "$repo" rev-parse --short "origin/$branch"); the client gets origin/$branch." >&2
fi

echo "Updating $host ($dir) to origin/$branch"
# shellcheck disable=SC2029  # $dir, $branch and the arguments expand here on purpose.
ssh -o BatchMode=yes "$host" "set -eu
cd $dir
git fetch --quiet origin $branch
git checkout --quiet $branch
git merge --quiet --ff-only origin/$branch
packaging/sub-api-proxy/update-local.sh --role router $*
sleep 1
systemctl --user is-active sap-router
curl -s -m 5 http://127.0.0.1:8317/sap/router/status; echo"
