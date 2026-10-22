use sap_cli::{
    setup::{self, CodingClient},
    table,
};
use sap_core::api::FlowSnapshot;
use std::{fs, process::Command};

#[test]
fn setup_preserves_settings_and_undo_restores_exact_bytes() {
    let temp = tempfile::tempdir().unwrap();
    for (client, filename, original) in [
        (
            CodingClient::Claude,
            "settings.json",
            "{\"theme\":\"dark\",\"env\":{\"KEEP\":\"yes\"}}\n",
        ),
        (
            CodingClient::Codex,
            "config.toml",
            "# keep this comment\nmodel = \"test-model\" # inline comment\n[model_providers.existing]\nname = \"Existing\"\n",
        ),
    ] {
        let path = temp.path().join(filename);
        fs::write(&path, original).unwrap();
        assert!(
            setup::setup(&path, client, "http://127.0.0.1:8317", Some("client-test"), true)
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
        let backup = setup::setup(&path, client, "http://127.0.0.1:8317", Some("client-test"), false)
            .unwrap()
            .unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
        setup::points_at_router(&path, client, "http://127.0.0.1:8317").unwrap();
        let changed = fs::read_to_string(&path).unwrap();
        match client {
            CodingClient::Claude => {
                let value: serde_json::Value = serde_json::from_str(&changed).unwrap();
                assert_eq!(value["theme"], "dark");
                assert_eq!(value["env"]["KEEP"], "yes");
                assert_eq!(value["env"]["ANTHROPIC_AUTH_TOKEN"], "client-test");
            }
            CodingClient::Codex => {
                for retained in [
                    "# keep this comment",
                    "# inline comment",
                    "model = \"test-model\"",
                    "[model_providers.existing]",
                ] {
                    assert!(changed.contains(retained));
                }
            }
        }
        setup::undo(&path, true).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), changed);
        setup::undo(&path, false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn undo_uses_latest_backup_and_remembers_absent_files() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("nested/config.toml");
    setup::setup(&path, CodingClient::Codex, "http://127.0.0.1:8317", None, false).unwrap();
    let first = fs::read_to_string(&path).unwrap();
    setup::setup(&path, CodingClient::Codex, "http://127.0.0.1:9000", None, false).unwrap();
    setup::undo(&path, false).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), first);
    setup::undo(&path, false).unwrap();
    assert!(!path.exists());
}

#[test]
fn invalid_settings_are_left_untouched() {
    let temp = tempfile::tempdir().unwrap();
    for (client, original) in [
        (CodingClient::Claude, "{\"env\":false}"),
        (CodingClient::Codex, "model_providers = false\n"),
    ] {
        let path = temp.path().join("settings");
        fs::write(&path, original).unwrap();
        assert!(setup::setup(&path, client, "http://127.0.0.1:8317", Some("test"), false).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), original);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}

#[test]
fn init_never_overwrites() {
    let temp = tempfile::tempdir().unwrap();
    for server in [false, true] {
        let path = temp.path().join(if server { "sap.yaml" } else { "router.yaml" });
        sap_cli::init(&path, server).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(sap_cli::init(&path, server).is_err());
        assert_eq!(fs::read(path).unwrap(), original);
    }
}

#[test]
fn fixture_tables_and_status_are_readable() {
    let flow: FlowSnapshot = serde_json::from_str(include_str!("../../sap-core/tests/fixtures/flow.json")).unwrap();
    let rendered = table::flow(&flow);
    assert!(rendered.contains("NODE"));
    assert!(rendered.contains("power-server"));
    assert!(rendered.contains("6 accounts, 10 active threads, 0 open conflicts"));
    assert!(rendered.contains("3177"));
    assert!(table::render(&flow.threads, table::THREADS).contains("parent affinity"));
    assert!(table::render(&flow.accounts, table::ACCOUNTS).contains("exhausted"));
    assert!(
        !table::render(&serde_json::json!([{"name":"bad\u{1b}[31m\nrow"}]), &[("NAME", "name")]).contains('\u{1b}')
    );
}

#[tokio::test]
async fn status_command_uses_local_http_and_management_auth() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
            assert!(request.len() < 8192);
        }
        let text = String::from_utf8(request).unwrap();
        assert!(text.starts_with("GET /sap/api/flow HTTP/1.1"));
        assert!(text.contains("authorization: Bearer management-test\r\n"));
        let body = include_str!("../../sap-core/tests/fixtures/flow.json");
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let home = temp.path().to_owned();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let output = tokio::task::spawn_blocking(move || {
            Command::new(env!("CARGO_BIN_EXE_sap"))
                .args([
                    "--router",
                    &format!("http://{address}"),
                    "--key",
                    "management-test",
                    "--home",
                ])
                .arg(home)
                .args(["status", "--json"])
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["stats"]["requests_3h"], 3177);
        server.await.unwrap();
    })
    .await;
    assert!(result.is_ok());
}

#[test]
fn codex_updates_existing_provider_without_losing_comments() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let original = "model_provider = \"old\" # selected provider\n[model_providers.sub-api-proxy]\nbase_url = \"http://127.0.0.1:9/v1\" # endpoint\ncustom = true\n";
    fs::write(&path, original).unwrap();
    setup::setup(&path, CodingClient::Codex, "http://127.0.0.1:8317", None, false).unwrap();
    let changed = fs::read_to_string(&path).unwrap();
    for text in ["# selected provider", "# endpoint", "custom = true"] {
        assert!(changed.contains(text));
    }
    setup::points_at_router(&path, CodingClient::Codex, "http://127.0.0.1:8317").unwrap();
    setup::undo(&path, false).unwrap();
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}
