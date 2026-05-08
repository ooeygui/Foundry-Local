// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Foundry ACP — Agent Communication Protocol server for Foundry Local.
//!
//! Exposes Foundry Local models as ACP-compatible agents, enabling any ACP
//! client to discover and invoke local AI models through the standard
//! Agent Connect Protocol REST API (v0.2.3).

use std::time::Duration;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use foundry_local_sdk::{FoundryLocalConfig, FoundryLocalManager};
use foundry_acp::ServerConfig;

/// Foundry ACP Server — Agent Communication Protocol for Foundry Local
#[derive(Parser, Debug)]
#[command(name = "foundry-acp", version, about)]
struct Cli {
    /// Host address to bind to
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to listen on
    #[arg(long, default_value_t = 8088)]
    port: u16,

    /// Model alias to load on startup (loads all catalog models if omitted)
    #[arg(long)]
    model: Option<String>,

    /// Maximum run timeout in seconds (0 = unlimited)
    #[arg(long, default_value_t = 300)]
    timeout: u64,

    /// Maximum number of concurrent runs (0 = unlimited)
    #[arg(long, default_value_t = 64)]
    max_concurrent_runs: usize,

    /// Path to SQLite database for persistent storage (omit for in-memory only)
    #[arg(long)]
    db: Option<std::path::PathBuf>,

    /// TLS certificate file (PEM format)
    #[cfg(feature = "tls")]
    #[arg(long, requires = "key")]
    cert: Option<std::path::PathBuf>,

    /// TLS private key file (PEM format)
    #[cfg(feature = "tls")]
    #[arg(long, requires = "cert")]
    key: Option<std::path::PathBuf>,

    /// Install as a Windows service
    #[cfg(all(windows, feature = "windows-service"))]
    #[arg(long)]
    install_service: bool,

    /// Uninstall the Windows service
    #[cfg(all(windows, feature = "windows-service"))]
    #[arg(long)]
    uninstall_service: bool,

    /// Run as a Windows service (used internally by SCM)
    #[cfg(all(windows, feature = "windows-service"))]
    #[arg(long, hide = true)]
    run_as_service: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // Handle Windows service commands
    #[cfg(all(windows, feature = "windows-service"))]
    {
        if cli.install_service {
            let exe = std::env::current_exe()?.display().to_string();
            return foundry_acp::service::win_svc::install_service(&exe).map_err(Into::into);
        }
        if cli.uninstall_service {
            return foundry_acp::service::win_svc::uninstall_service().map_err(Into::into);
        }
        if cli.run_as_service {
            return foundry_acp::service::win_svc::run_as_service().map_err(Into::into);
        }
    }

    // ── Initialise Foundry Local SDK ──────────────────────────────────
    tracing::info!("Initializing Foundry Local SDK...");
    let manager = FoundryLocalManager::create(FoundryLocalConfig::new("foundry_acp"))?;
    tracing::info!("SDK initialized");

    let config = ServerConfig {
        run_timeout: if cli.timeout == 0 {
            Duration::from_secs(u64::MAX)
        } else {
            Duration::from_secs(cli.timeout)
        },
        max_concurrent_runs: cli.max_concurrent_runs,
        db_path: cli.db,
    };

    let state =
        foundry_acp::init_state_with_config(manager, cli.model.as_deref(), config).await?;
    tracing::info!("Registered {} ACP agents", state.agents.len());

    for (id, entry) in &state.agents {
        tracing::info!(
            "  Agent: {} (alias={}, id={}, loaded={})",
            entry.agent.metadata.agent_ref.name,
            entry.model_alias,
            id,
            entry.is_loaded
        );
    }

    let app = foundry_acp::build_router(state);

    // ── Start server ─────────────────────────────────────────────────
    let addr = format!("{}:{}", cli.host, cli.port);

    #[cfg(feature = "tls")]
    if let (Some(cert_path), Some(key_path)) = (cli.cert.as_ref(), cli.key.as_ref()) {
        tracing::info!("Foundry ACP server listening on https://{addr} (TLS)");
        tracing::info!("ACP spec version: 0.2.3");

        let tls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_path, key_path)
            .await?;
        axum_server::bind_rustls(addr.parse()?, tls_config)
            .serve(app.into_make_service())
            .await?;
        return Ok(());
    }

    tracing::info!("Foundry ACP server listening on http://{addr}");
    tracing::info!("ACP spec version: 0.2.3");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");
    tracing::info!("Shutdown signal received");
}
