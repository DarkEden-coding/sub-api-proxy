use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use http::Method;
use sap_cli::{
    client::{Client, management_key, segment},
    setup::{self, CodingClient},
    table,
};
use sap_core::{
    api::*,
    health::HealthReport,
    ids::{AccountId, NodeId, Provider},
    sync::Conflict,
};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Manage Sub-Api-Proxy and configure coding clients")]
struct Cli {
    #[arg(long, global = true, default_value = "http://127.0.0.1:8317")]
    router: String,
    #[arg(long, global = true)]
    key: Option<String>,
    #[arg(long, global = true)]
    json: bool,
    /// Override the home directory for all CLI config paths.
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Status,
    Accounts {
        #[command(subcommand)]
        action: Option<AccountAction>,
    },
    Threads {
        #[arg(long)]
        all: bool,
    },
    Nodes,
    Events {
        #[arg(short = 'n', default_value_t = 20)]
        limit: usize,
    },
    Conflicts {
        #[command(subcommand)]
        action: Option<ConflictAction>,
    },
    Login {
        #[arg(value_enum)]
        provider: CodingClient,
    },
    Assign {
        thread_key: String,
        account: String,
    },
    Setup {
        #[arg(value_enum)]
        client: CodingClient,
        #[arg(long)]
        undo: bool,
        #[arg(long)]
        dry_run: bool,
        /// Explicit settings path; useful for isolated configuration and tests.
        #[arg(long)]
        path: Option<PathBuf>,
    },
    Doctor {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    Init {
        #[arg(value_enum)]
        kind: ConfigKind,
        #[arg(long)]
        path: Option<PathBuf>,
    },
}
#[derive(Clone, Copy, ValueEnum)]
enum ConfigKind {
    Router,
    Server,
}
#[derive(Subcommand)]
enum AccountAction {
    Disable {
        id: String,
    },
    Enable {
        id: String,
    },
    Priority {
        id: String,
        #[arg(allow_hyphen_values = true)]
        n: i32,
    },
}
#[derive(Subcommand)]
enum ConflictAction {
    Resolve {
        id: String,
        #[arg(long)]
        keep: String,
    },
}

fn output(value: &impl Serialize, columns: &[(&str, &str)], json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        print!("{}", table::render(value, columns));
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("sap: {error:#}");
        std::process::exit(1);
    }
}
async fn run(cli: Cli) -> Result<()> {
    let home = cli
        .home
        .clone()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .context("HOME is unset; use --home")?;
    let config_dir = home.join(".config/sub-api-proxy");
    match cli.command {
        Command::Setup {
            client,
            undo,
            dry_run,
            path,
        } => {
            let path = path.unwrap_or_else(|| setup::settings_path(&home, client));
            // Validate the URL before writing it to client settings.
            Client::new(&cli.router, None)?;
            let backup = if undo {
                Some(setup::undo(&path, dry_run)?)
            } else {
                setup::setup(&path, client, &cli.router, dry_run)?
            };
            output(
                &serde_json::json!({"path": path, "backup": backup, "dry_run": dry_run, "action": if undo {"restore"} else {"configure"}}),
                &[
                    ("ACTION", "action"),
                    ("PATH", "path"),
                    ("BACKUP", "backup"),
                    ("DRY RUN", "dry_run"),
                ],
                cli.json,
            )?;
            if matches!(client, CodingClient::Codex) && !cli.json && !undo {
                println!("Set SAP_CLIENT_KEY in the environment used to launch Codex.");
            }
            return Ok(());
        }
        Command::Init { kind, path } => {
            let server = matches!(kind, ConfigKind::Server);
            let path = path.unwrap_or_else(|| PathBuf::from(if server { "sap.yaml" } else { "router.yaml" }));
            sap_cli::init(&path, server)?;
            output(
                &serde_json::json!({"created": path}),
                &[("CREATED", "created")],
                cli.json,
            )?;
            return Ok(());
        }
        Command::Doctor { config } => {
            return doctor(
                &cli.router,
                cli.key,
                &home,
                config.unwrap_or_else(|| config_dir.join("router.yaml")),
                cli.json,
            )
            .await;
        }
        _ => {}
    }
    let client = Client::new(
        &cli.router,
        Some(management_key(cli.key, &config_dir.join("management-key"))?),
    )?;
    match cli.command {
        Command::Status => {
            let flow: FlowSnapshot = client.get("/sap/api/flow").await?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&flow)?);
            } else {
                print!("{}", table::flow(&flow));
            }
        }
        Command::Accounts { action } => {
            if let Some(action) = action {
                let (id, patch) = match action {
                    AccountAction::Disable { id } => (
                        id,
                        AccountPatch {
                            disabled: Some(true),
                            ..Default::default()
                        },
                    ),
                    AccountAction::Enable { id } => (
                        id,
                        AccountPatch {
                            disabled: Some(false),
                            ..Default::default()
                        },
                    ),
                    AccountAction::Priority { id, n } => (
                        id,
                        AccountPatch {
                            priority: Some(n),
                            ..Default::default()
                        },
                    ),
                };
                let value: AccountView = client
                    .request(
                        Method::PATCH,
                        &format!("/sap/api/accounts/{}", segment(&id)),
                        Some(&patch),
                    )
                    .await?;
                output(&value, table::ACCOUNTS, cli.json)?;
            } else {
                output(
                    &client.get::<Vec<AccountView>>("/sap/api/accounts").await?,
                    table::ACCOUNTS,
                    cli.json,
                )?;
            }
        }
        Command::Threads { all } => output(
            &client
                .get::<Vec<ThreadView>>(&format!("/sap/api/threads?active={}", !all))
                .await?,
            table::THREADS,
            cli.json,
        )?,
        Command::Nodes => output(
            &client.get::<Vec<NodeView>>("/sap/api/nodes").await?,
            table::NODES,
            cli.json,
        )?,
        Command::Events { limit } => output(
            &client
                .get::<Vec<EventView>>(&format!("/sap/api/events?limit={limit}"))
                .await?,
            table::EVENTS,
            cli.json,
        )?,
        Command::Conflicts { action } => {
            if let Some(ConflictAction::Resolve { id, keep }) = action {
                let value: Conflict = client
                    .request(
                        Method::POST,
                        &format!("/sap/api/conflicts/{}/resolve", segment(&id)),
                        Some(&ResolveRequest { keep: NodeId(keep) }),
                    )
                    .await?;
                output(&value, table::CONFLICTS, cli.json)?;
            } else {
                output(
                    &client.get::<Vec<Conflict>>("/sap/api/conflicts").await?,
                    table::CONFLICTS,
                    cli.json,
                )?;
            }
        }
        Command::Assign { thread_key, account } => {
            let value: ThreadView = client
                .request(
                    Method::POST,
                    &format!("/sap/api/threads/{}/assign", segment(&thread_key)),
                    Some(&AssignRequest {
                        account: AccountId(account),
                    }),
                )
                .await?;
            output(&value, table::THREADS, cli.json)?;
        }
        Command::Login { provider } => {
            let value: LoginStart = client
                .request(
                    Method::POST,
                    "/sap/api/accounts/login",
                    Some(&LoginRequest {
                        provider: match provider {
                            CodingClient::Claude => Provider::Claude,
                            CodingClient::Codex => Provider::Codex,
                        },
                        account: None,
                    }),
                )
                .await?;
            output(&value, &[("LOGIN URL", "url"), ("LOGIN ID", "login_id")], cli.json)?;
        }
        Command::Setup { .. } | Command::Doctor { .. } | Command::Init { .. } => unreachable!(),
    }
    Ok(())
}

#[derive(Serialize)]
struct Check {
    check: String,
    pass: bool,
    result: &'static str,
    detail: String,
}
fn check(name: impl Into<String>, result: Result<()>) -> Check {
    Check {
        check: name.into(),
        pass: result.is_ok(),
        result: if result.is_ok() { "PASS" } else { "FAIL" },
        detail: result.err().map(|e| e.to_string()).unwrap_or_else(|| "ok".into()),
    }
}
async fn doctor(router: &str, key: Option<String>, home: &std::path::Path, config: PathBuf, json: bool) -> Result<()> {
    let mut checks = Vec::new();
    let parsed = sap_router::config::Config::load(Some(config));
    checks.push(check(
        "router config",
        parsed.as_ref().map(|_| ()).map_err(|e| anyhow::anyhow!("{e}")),
    ));
    let local = Client::new(router, None)?;
    let status = local
        .get::<serde_json::Value>("/sap/router/status")
        .await
        .and_then(|v| {
            if v["servers"].is_array() && v["preferred"].is_string() {
                Ok(())
            } else {
                bail!("router has no preferred healthy server")
            }
        });
    checks.push(check("router status", status));
    if let Ok(config) = parsed {
        for server in config.servers {
            let result = match Client::new(&server.base_url, None) {
                Ok(client) => client.get::<HealthReport>("/sap/health").await.and_then(|h| {
                    if h.state == sap_core::health::NodeState::Serving {
                        Ok(())
                    } else {
                        bail!("server is not serving")
                    }
                }),
                Err(error) => Err(error),
            };
            checks.push(check(format!("server {} health", server.name), result));
        }
    }
    for (name, client) in [
        ("Claude settings", CodingClient::Claude),
        ("Codex settings", CodingClient::Codex),
    ] {
        checks.push(check(
            name,
            setup::points_at_router(&setup::settings_path(home, client), client, router),
        ));
    }
    let auth = match management_key(key, &home.join(".config/sub-api-proxy/management-key")) {
        Ok(key) => Client::new(router, Some(key))?
            .get::<Vec<NodeView>>("/sap/api/nodes")
            .await
            .map(|_| ()),
        Err(error) => Err(error),
    };
    checks.push(check("management key", auth));
    output(
        &checks,
        &[("CHECK", "check"), ("RESULT", "result"), ("DETAIL", "detail")],
        json,
    )?;
    if checks.iter().any(|c| !c.pass) {
        bail!("doctor found failing checks");
    }
    Ok(())
}
