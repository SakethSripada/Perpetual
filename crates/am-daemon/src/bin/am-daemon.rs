//! Standalone AgentManager daemon process.
//!
//! Hosts `am-core` and serves it over a localhost TCP socket. On startup it
//! writes `<data_dir>/daemon.json` (`{ "port", "token" }`) so a local client
//! can discover and authenticate to the running instance. Runs until Ctrl-C,
//! then shuts the core down so no agent processes leak.
//!
//! Config via env: `AM_DATA_DIR` (data/db/worktrees root), `AM_DAEMON_PORT`
//! (defaults to 0 = OS-assigned).

use std::path::PathBuf;

use am_core::AppCore;
use am_daemon::{generate_token, Server};
use am_mcp::{McpPolicy, MCP_TOKEN_ENV, MCP_URL_ENV};
use serde::Deserialize;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,am_daemon=debug,am_core=debug".into()),
        )
        .init();

    let data_dir = data_dir();
    if std::env::args().nth(1).as_deref() == Some("mcp-stdio") {
        if let Err(err) = run_mcp_stdio(data_dir).await {
            tracing::error!(error = %err, "MCP stdio bridge failed");
            std::process::exit(1);
        }
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        tracing::error!(?data_dir, error = %e, "failed to create data dir");
        std::process::exit(1);
    }
    let port: u16 = std::env::var("AM_DAEMON_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let mcp_port: u16 = std::env::var("AM_MCP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);

    let core = match AppCore::new(&data_dir).await {
        Ok(core) => core,
        Err(e) => {
            tracing::error!(error = %e, "failed to initialize core");
            std::process::exit(1);
        }
    };

    let token = generate_token();
    let server = match Server::bind(core.clone(), token.clone(), port).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to bind daemon socket");
            std::process::exit(1);
        }
    };
    let addr = server.addr();

    let mcp_token = generate_token();
    let mcp =
        match am_mcp::serve_http(core.clone(), mcp_token, McpPolicy::default(), mcp_port).await {
            Ok(handle) => handle,
            Err(e) => {
                tracing::error!(error = %e, "failed to bind MCP listener");
                std::process::exit(1);
            }
        };
    core.set_mcp_endpoint(mcp.endpoint.url.clone(), mcp.endpoint.token.clone())
        .await;

    let endpoint = data_dir.join("daemon.json");
    let body = serde_json::json!({
        "port": addr.port(),
        "token": token,
        "mcp_port": mcp.addr.port(),
        "mcp_url": mcp.endpoint.url.clone(),
        "mcp_token": mcp.endpoint.token.clone(),
    })
    .to_string();
    if let Err(e) = std::fs::write(&endpoint, body) {
        tracing::warn!(?endpoint, error = %e, "failed to write endpoint file");
    }
    tracing::info!(%addr, mcp_addr = %mcp.addr, ?endpoint, "AgentManager daemon listening");

    let power = am_daemon::power::spawn(core.clone());
    let serve = tokio::spawn(server.serve());

    tokio::select! {
        _ = tokio::signal::ctrl_c() => tracing::info!("shutdown signal received"),
        _ = wait_for_terminate() => tracing::info!("terminate signal received"),
        _ = wait_forever(&serve) => {}
    }

    // Shutdown handoff must run before the daemon tears down its sessions.
    // The extension also calls this RPC during normal deactivation; this path
    // covers OS termination and direct daemon shutdowns.
    let _ = core.prepare_shutdown().await;
    power.shutdown().await;
    serve.abort();
    core.clear_mcp_endpoint().await;
    mcp.shutdown().await;
    core.shutdown().await;
    let _ = std::fs::remove_file(&endpoint);
    tracing::info!("daemon stopped");
}

/// Resolve the data directory: `AM_DATA_DIR`, else `~/.agentmanager`.
fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("AM_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".agentmanager")
}

/// Completes only if the serve task ends on its own (a fatal accept error).
async fn wait_forever(serve: &tokio::task::JoinHandle<()>) {
    if serve.is_finished() {
        return;
    }
    // The accept loop normally never returns; poll cheaply so `select!` has a
    // second branch without consuming the JoinHandle.
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        if serve.is_finished() {
            return;
        }
    }
}

async fn wait_for_terminate() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut signal) = signal(SignalKind::terminate()) {
            let _ = signal.recv().await;
            return;
        }
    }
    std::future::pending::<()>().await;
}

#[derive(Debug, Deserialize)]
struct EndpointFile {
    #[serde(default)]
    mcp_url: Option<String>,
    #[serde(default)]
    mcp_port: Option<u16>,
    #[serde(default)]
    mcp_token: Option<String>,
}

async fn run_mcp_stdio(data_dir: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let file = read_endpoint_file(&data_dir);
    let url = std::env::var(MCP_URL_ENV)
        .or_else(|_| std::env::var("AM_MCP_URL"))
        .ok()
        .or_else(|| file.as_ref().and_then(|file| file.mcp_url.clone()))
        .or_else(|| {
            file.as_ref()
                .and_then(|file| file.mcp_port)
                .map(|port| format!("http://127.0.0.1:{port}/mcp"))
        })
        .ok_or("missing MCP URL; start am-daemon first or set AGENTMANAGER_MCP_URL")?;
    let token = std::env::var(MCP_TOKEN_ENV)
        .ok()
        .or_else(|| file.and_then(|file| file.mcp_token))
        .ok_or("missing MCP token; start am-daemon first or set AGENTMANAGER_MCP_TOKEN")?;
    am_mcp::stdio_bridge(url, token).await?;
    Ok(())
}

fn read_endpoint_file(data_dir: &std::path::Path) -> Option<EndpointFile> {
    let path = data_dir.join("daemon.json");
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}
