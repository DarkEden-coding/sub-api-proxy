//! Content-addressed native images (Go v8.0.35 `internal/pluginhost/shadow.go`).
//! Images and their callback tables remain resident after logical unload, so each
//! distinct build needs its own path and the source artifact must stay replaceable.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Once;

use sha2::{Digest, Sha256};

const PREFIX: &str = "cliproxy-plugin-";
const TEMP_PREFIX: &str = ".cliproxy-plugin-";

pub(crate) fn copy(source: &Path, id: &str) -> Result<PathBuf, String> {
    if !crate::platform::valid_id(id) {
        return Err("invalid plugin id".into());
    }
    let dir = std::env::temp_dir()
        .join("cliproxy-pluginhost")
        .join(format!("pid-{}", std::process::id()));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|e| e.to_string())?;
    static CLEANUP: Once = Once::new();
    CLEANUP.call_once(|| {
        remove_stale_files(&dir);
        remove_stale_dirs(dir.parent().expect("process directory has a parent"));
    });
    copy_to(source, id, &dir)
}

fn copy_to(source: &Path, id: &str, dir: &Path) -> Result<PathBuf, String> {
    let (mut temp, temp_path) = crate::store::create_temp(dir, &format!("{PREFIX}{id}"))?;
    let result = (|| {
        let mut input = File::open(source).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut size = 0;
        let mut buf = [0; 64 * 1024];
        loop {
            let len = input.read(&mut buf).map_err(|e| e.to_string())?;
            if len == 0 {
                break;
            }
            temp.write_all(&buf[..len]).map_err(|e| e.to_string())?;
            hash.update(&buf[..len]);
            size += len as u64;
        }
        drop(temp);
        let digest = hash.finalize();
        let extension = source
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let digest_hex = format!("{digest:x}");
        let target = dir.join(format!("{PREFIX}{id}-{}{extension}", &digest_hex[..32]));
        if matches(&target, size, &digest) {
            return Ok(target);
        }
        // Go os.Remove also removes an empty directory, never its contents.
        let removed = fs::remove_file(&target).or_else(|unlink_error| {
            fs::remove_dir(&target).map_err(|dir_error| {
                if dir_error.raw_os_error() == Some(libc::ENOTDIR) {
                    unlink_error
                } else {
                    dir_error
                }
            })
        });
        if let Err(e) = removed
            && e.kind() != std::io::ErrorKind::NotFound
        {
            if matches(&target, size, &digest) {
                return Ok(target);
            }
            return Err(format!("remove stale shadow plugin: {e}"));
        }
        if let Err(e) = fs::rename(&temp_path, &target) {
            if matches(&target, size, &digest) {
                return Ok(target);
            }
            return Err(format!("move shadow plugin: {e}"));
        }
        Ok(target)
    })();
    let _ = fs::remove_file(temp_path);
    result
}

fn matches(path: &Path, size: u64, digest: &[u8]) -> bool {
    // Check before opening: opening a FIFO would block instead of repairing it.
    if !fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() == size) {
        return false;
    }
    let Ok(mut file) = File::open(path) else { return false };
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).is_ok() && hash.finalize().as_slice() == digest
}

fn remove_stale_files(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !entry.file_type().is_ok_and(|t| t.is_dir()) && (name.starts_with(PREFIX) || name.starts_with(TEMP_PREFIX)) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn remove_stale_dirs(base: &Path) {
    let Ok(entries) = fs::read_dir(base) else { return };
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|n| n.strip_prefix("pid-"))
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if pid <= 0 || pid as u32 == std::process::id() {
            continue;
        }
        // Only ESRCH proves the process has exited; EPERM also means keep it.
        // SAFETY: signal zero checks process liveness without sending a signal.
        if unsafe { libc::kill(pid, 0) } == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_addressed_copies_reuse_change_and_repair() {
        // Go TestShadowCopyPlugin{ReusesContentAddressedShadow,
        // CreatesNewPathForChangedContent,ReplacesCorruptSameSizeShadow}.
        let root = std::env::temp_dir().join(format!("cpa-shadow-content-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("alpha.so");
        fs::write(&source, b"plugin-v1").unwrap();
        let first = copy_to(&source, "alpha", &root).unwrap();
        // Independently calculated SHA-256 of plugin-v1, first 128 bits.
        assert_eq!(
            first.file_name().unwrap(),
            "cliproxy-plugin-alpha-d3fee1b1103ae4a2e7fd52a5939b5a04.so"
        );
        assert_eq!(copy_to(&source, "alpha", &root).unwrap(), first);
        fs::write(&first, b"corrupt!!").unwrap();
        assert_eq!(copy_to(&source, "alpha", &root).unwrap(), first);
        assert_eq!(fs::read(&first).unwrap(), b"plugin-v1");
        fs::remove_file(&first).unwrap();
        fs::create_dir(&first).unwrap();
        fs::write(first.join("keep"), b"x").unwrap();
        let error = copy_to(&source, "alpha", &root).unwrap_err();
        assert!(
            error.ends_with(&std::io::Error::from_raw_os_error(libc::ENOTEMPTY).to_string()),
            "{error}"
        );
        assert_eq!(
            fs::read(first.join("keep")).unwrap(),
            b"x",
            "nonempty directories are never removed"
        );
        fs::remove_file(first.join("keep")).unwrap();
        assert_eq!(
            copy_to(&source, "alpha", &root).unwrap(),
            first,
            "empty directory is repaired"
        );
        fs::remove_file(&first).unwrap();
        let fifo = std::ffi::CString::new(first.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path in this test's private directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let (tx, rx) = std::sync::mpsc::channel();
        let (src, dir) = (source.clone(), root.clone());
        let repair = std::thread::spawn(move || tx.send(copy_to(&src, "alpha", &dir)).unwrap());
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(2))
                .expect("FIFO repair must not block")
                .unwrap(),
            first
        );
        repair.join().unwrap();
        assert_eq!(fs::read(&first).unwrap(), b"plugin-v1");
        fs::write(&source, b"plugin-v2").unwrap();
        let second = copy_to(&source, "alpha", &root).unwrap();
        assert_ne!(first, second);
        assert_eq!(fs::read(&first).unwrap(), b"plugin-v1");
        assert_eq!(fs::read(&second).unwrap(), b"plugin-v2");
        assert!(copy_to(&root.join("missing.so"), "alpha", &root).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 3, "temporary files are removed");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleanup_preserves_unrelated_files_and_live_processes() {
        use std::process::{Command, Stdio};
        // Go TestRemoveStaleShadowPluginsOnlyRemovesShadowFiles and
        // TestRemoveStaleShadowPluginDirsOnlyRemovesDeadProcesses.
        let root = std::env::temp_dir().join(format!("cpa-shadow-cleanup-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for name in [
            "cliproxy-plugin-alpha-old.so",
            ".cliproxy-plugin-alpha.tmp-old",
            "keep.so",
        ] {
            fs::write(root.join(name), b"x").unwrap();
        }
        remove_stale_files(&root);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        assert!(root.join("keep.so").is_file());
        let mut live = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let mut dead = Command::new("true").spawn().unwrap();
        dead.wait().unwrap();
        let current_dir = root.join(format!("pid-{}", std::process::id()));
        let live_dir = root.join(format!("pid-{}", live.id()));
        let dead_dir = root.join(format!("pid-{}", dead.id()));
        for dir in [
            &current_dir,
            &live_dir,
            &dead_dir,
            &root.join("pid-invalid"),
            &root.join("unrelated"),
        ] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("keep.so"), b"x").unwrap();
        }
        remove_stale_dirs(&root);
        assert!(!dead_dir.exists());
        for dir in [
            &current_dir,
            &live_dir,
            &root.join("pid-invalid"),
            &root.join("unrelated"),
        ] {
            assert!(dir.join("keep.so").is_file());
        }
        drop(live.stdin.take());
        live.wait().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
