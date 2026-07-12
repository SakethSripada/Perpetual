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

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,am_daemon=debug,am_core=debug".into()),
        )
        .init();

    let data_dir = data_dir();
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        tracing::error!(?data_dir, error = %e, "failed to create data dir");
        std::process::exit(1);
    }
    let port: u16 = std::env::var("AM_DAEMON_PORT")
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

    let endpoint = data_dir.join("daemon.json");
    let body = serde_json::json!({
        "port": addr.port(),
        "token": token,
    })
    .to_string();
    if let Err(e) = std::fs::write(&endpoint, body) {
        tracing::warn!(?endpoint, error = %e, "failed to write endpoint file");
    }
    tracing::info!(%addr, ?endpoint, "AgentManager daemon listening");

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
