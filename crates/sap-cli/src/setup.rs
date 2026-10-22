use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use toml_edit::{DocumentMut, Item, value};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CodingClient {
    Claude,
    Codex,
}

pub fn settings_path(home: &Path, client: CodingClient) -> PathBuf {
    home.join(match client {
        CodingClient::Claude => ".claude/settings.json",
        CodingClient::Codex => ".codex/config.toml",
    })
}

/// The provisional Codex provider configuration. Keep phase-0 changes here.
// ponytail: custom-provider semantics await phase-0 validation; replace this function
// with the validated ChatGPT-mode configuration when that workstream finishes.
pub fn configure_codex(document: &mut DocumentMut, router: &str) -> Result<()> {
    if let Some(providers) = document.get("model_providers") {
        ensure!(providers.is_table_like(), "model_providers must be a TOML table");
        if let Some(provider) = providers.get("sub-api-proxy") {
            ensure!(provider.is_table_like(), "sub-api-proxy provider must be a TOML table");
        }
    }
    set_string(&mut document["model_provider"], "sub-api-proxy");
    let provider = &mut document["model_providers"]["sub-api-proxy"];
    set_string(&mut provider["name"], "Sub-Api-Proxy");
    set_string(
        &mut provider["base_url"],
        format!("{}/v1", router.trim_end_matches('/')),
    );
    set_string(&mut provider["wire_api"], "responses");
    set_string(&mut provider["env_key"], "SAP_CLIENT_KEY");
    Ok(())
}

fn set_string(item: &mut Item, text: impl Into<String>) {
    let mut replacement = value(text.into());
    if let (Some(old), Some(new)) = (item.as_value(), replacement.as_value_mut()) {
        *new.decor_mut() = old.decor().clone();
    }
    *item = replacement;
}

/// Edits explicit paths only. Dry runs validate input without creating any files.
pub fn setup(
    path: &Path,
    client: CodingClient,
    router: &str,
    token: Option<&str>,
    dry_run: bool,
) -> Result<Option<PathBuf>> {
    let original = match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let updated = match client {
        CodingClient::Claude => {
            let token = token.context("Claude setup needs --client-key or SAP_CLIENT_KEY")?;
            ensure!(!token.is_empty(), "client key is empty");
            let mut doc: Value = serde_json::from_str(original.as_deref().unwrap_or("{}"))?;
            ensure!(doc.is_object(), "Claude settings must be a JSON object");
            if doc.get("env").is_none() {
                doc["env"] = serde_json::json!({});
            }
            ensure!(doc["env"].is_object(), "Claude env must be a JSON object");
            doc["env"]["ANTHROPIC_BASE_URL"] = router.trim_end_matches('/').into();
            doc["env"]["ANTHROPIC_AUTH_TOKEN"] = token.into();
            format!("{}\n", serde_json::to_string_pretty(&doc)?)
        }
        CodingClient::Codex => {
            let mut doc: DocumentMut = original
                .as_deref()
                .unwrap_or("")
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid Codex TOML settings"))?;
            configure_codex(&mut doc, router)?;
            doc.to_string()
        }
    };
    if dry_run {
        return Ok(None);
    }
    let parent = path.parent().context("settings path needs a parent")?;
    fs::create_dir_all(parent)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let backup = path.with_file_name(format!(
        "{}.sap-backup-{stamp}.{}",
        path.file_name().context("missing filename")?.to_string_lossy(),
        if original.is_some() { "saved" } else { "absent" }
    ));
    // A marker records that undo must remove a newly created config.
    write_new(&backup, original.as_deref().unwrap_or("").as_bytes())?;
    atomic_write(path, updated.as_bytes())?;
    Ok(Some(backup))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().context("missing parent")?)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub fn undo(path: &Path, dry_run: bool) -> Result<PathBuf> {
    let prefix = format!(
        "{}.sap-backup-",
        path.file_name().context("missing filename")?.to_string_lossy()
    );
    let mut backups = fs::read_dir(path.parent().context("missing parent")?)?
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(&prefix))
                && matches!(p.extension().and_then(|s| s.to_str()), Some("saved" | "absent"))
        })
        .collect::<Vec<_>>();
    backups.sort();
    let backup = backups.pop().context("no setup backup found")?;
    if !dry_run {
        if backup.extension().is_some_and(|s| s == "absent") {
            if path.exists() {
                fs::remove_file(path)?;
            }
        } else {
            atomic_write(path, &fs::read(&backup)?)?;
        }
        fs::remove_file(&backup)?;
    }
    Ok(backup)
}

pub fn points_at_router(path: &Path, client: CodingClient, router: &str) -> Result<()> {
    let text = fs::read_to_string(path)?;
    match client {
        CodingClient::Claude => {
            let doc: Value = serde_json::from_str(&text)?;
            ensure!(
                doc["env"]["ANTHROPIC_BASE_URL"] == router.trim_end_matches('/'),
                "Claude base URL differs"
            );
            ensure!(
                doc["env"]["ANTHROPIC_AUTH_TOKEN"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty()),
                "Claude client key missing"
            );
        }
        CodingClient::Codex => {
            let doc: DocumentMut = text
                .parse()
                .map_err(|_| anyhow::anyhow!("invalid Codex TOML settings"))?;
            ensure!(
                doc.get("model_provider").and_then(|v| v.as_str()) == Some("sub-api-proxy"),
                "Codex provider differs"
            );
            let provider = doc
                .get("model_providers")
                .and_then(|v| v.get("sub-api-proxy"))
                .context("Codex provider missing")?;
            ensure!(
                provider.get("base_url").and_then(|v| v.as_str())
                    == Some(format!("{}/v1", router.trim_end_matches('/')).as_str()),
                "Codex base URL differs"
            );
            ensure!(
                provider.get("wire_api").and_then(|v| v.as_str()) == Some("responses")
                    && provider.get("env_key").and_then(|v| v.as_str()) == Some("SAP_CLIENT_KEY"),
                "Codex provider settings differ"
            );
        }
    }
    Ok(())
}
