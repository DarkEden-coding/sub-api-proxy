pub mod client;
pub mod setup;
pub mod table;

use anyhow::Result;
use std::{fs::OpenOptions, io::Write, path::Path};

pub const ROUTER_CONFIG: &str = "listen: 127.0.0.1:8317\nclient_host: workstation\nservers:\n  - name: primary\n    base_url: http://127.0.0.1:8318\n  - name: backup\n    base_url: http://127.0.0.1:8319\n";
pub const SERVER_CONFIG: &str = "node: power-server\nrole: primary\nstore_path: state.db\npeers: []\nrouting:\n  retry_deadline_secs: 60\n  compaction_move_margin: 0.25\n";

/// Creates a starter config without replacing an existing file.
pub fn init(path: &Path, server: bool) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(if server { SERVER_CONFIG } else { ROUTER_CONFIG }.as_bytes())?;
    Ok(())
}
