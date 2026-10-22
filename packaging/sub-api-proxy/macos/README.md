# macOS menu bar

Build and install after the server and router are configured:

```sh
sh packaging/sub-api-proxy/macos/build.sh
sh packaging/sub-api-proxy/macos/install.sh
```

The app lives in `~/Applications/Sub-Api-Proxy.app` and starts at login through
`io.sub-api-proxy.menubar`. Click its branching-arrow icon to open the local
admin panel. Right-click for its menu. Copy admin key reads the private local
management-key file and clears the clipboard after a minute if nothing else was
copied. Paste it into the panel's Settings page and save. Keys never enter URLs.

Quit menu bar app leaves the server and router running. Launch the app again
from Applications to restore the icon. Logs are under
`~/.local/state/sub-api-proxy/menubar.log`. Reinstalling preserves the previous
app bundle beside the new one with a timestamped backup suffix.
