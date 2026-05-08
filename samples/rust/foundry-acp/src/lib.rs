// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Foundry ACP — Agent Communication Protocol server for Foundry Local.
//!
//! This library crate exposes the server builder so that integration tests
//! can spin up a real ACP server in-process.

pub mod acp_types;
pub mod agents;
pub mod error;
pub mod health;
pub mod inference;
pub mod metrics;
pub mod openapi;
pub mod persistence;
pub mod runs;
pub mod service;
pub mod state;
pub mod threads;

use std::sync::Arc;
use std::time::Duration;

use axum::middleware as axum_mw;
use axum::routing::{get, post};
use axum::Router;
use dashmap::DashMap;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

use foundry_local_sdk::FoundryLocalManager;
use state::AppState;

/// Server configuration populated from CLI flags.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Maximum time a run may execute before being timed out.
    pub run_timeout: Duration,
    /// Maximum concurrent runs (0 = unlimited).
    pub max_concurrent_runs: usize,
    /// Path to SQLite database (None = in-memory only).
    pub db_path: Option<std::path::PathBuf>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            run_timeout: Duration::from_secs(300),
            max_concurrent_runs: 64,
            db_path: None,
        }
    }
}

/// Build the full ACP router from an already-initialised [`AppState`].
pub fn build_router(state: Arc<AppState>) -> Router {
    // Initialise metrics
    metrics::init_metrics();

    Router::new()
        // ── System ───────────────────────────────────────────────────
        .route("/health", get(health::health_check))
        .route("/metrics", get(metrics::metrics_handler))
        .route("/openapi.json", get(openapi::openapi_json))
        // ── Agents ───────────────────────────────────────────────────
        .route("/agents/search", post(agents::search_agents))
        .route("/agents/{agent_id}", get(agents::get_agent))
        .route(
            "/agents/{agent_id}/descriptor",
            get(agents::get_agent_descriptor),
        )
        // ── Stateless Runs ───────────────────────────────────────────
        .route("/runs", post(runs::create_stateless_run))
        .route("/runs/search", post(runs::search_stateless_runs))
        .route("/runs/wait", post(runs::create_and_wait_stateless))
        .route("/runs/stream", post(runs::create_and_stream_stateless))
        .route(
            "/runs/{run_id}",
            get(runs::get_stateless_run)
                .post(runs::resume_stateless_run)
                .delete(runs::delete_stateless_run),
        )
        .route("/runs/{run_id}/wait", get(runs::wait_stateless_run))
        .route("/runs/{run_id}/stream", get(runs::stream_stateless_run))
        .route("/runs/{run_id}/cancel", post(runs::cancel_stateless_run))
        // ── Threads ──────────────────────────────────────────────────
        .route("/threads", post(threads::create_thread))
        .route("/threads/search", post(threads::search_threads))
        .route(
            "/threads/{thread_id}",
            get(threads::get_thread)
                .patch(threads::patch_thread)
                .delete(threads::delete_thread),
        )
        .route("/threads/{thread_id}/copy", post(threads::copy_thread))
        .route(
            "/threads/{thread_id}/history",
            get(threads::get_thread_history),
        )
        // ── Thread Runs ──────────────────────────────────────────────
        .route(
            "/threads/{thread_id}/runs",
            get(threads::list_thread_runs).post(threads::create_thread_run),
        )
        .route(
            "/threads/{thread_id}/runs/wait",
            post(threads::create_and_wait_thread_run),
        )
        .route(
            "/threads/{thread_id}/runs/stream",
            post(threads::create_and_stream_thread_run),
        )
        .route(
            "/threads/{thread_id}/runs/{run_id}",
            get(threads::get_thread_run)
                .post(threads::resume_thread_run)
                .delete(threads::delete_thread_run),
        )
        .route(
            "/threads/{thread_id}/runs/{run_id}/wait",
            get(threads::wait_thread_run),
        )
        .route(
            "/threads/{thread_id}/runs/{run_id}/stream",
            get(threads::stream_thread_run),
        )
        .route(
            "/threads/{thread_id}/runs/{run_id}/cancel",
            post(threads::cancel_thread_run),
        )
        // ── Middleware ────────────────────────────────────────────────
        .layer(axum_mw::from_fn(metrics::track_metrics))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Create a fully initialised [`AppState`] from the Foundry SDK.
///
/// If `model_alias` is provided, only that model is downloaded/loaded.
/// Otherwise all catalog models are registered (but not necessarily loaded).
pub async fn init_state(
    manager: &'static FoundryLocalManager,
    model_alias: Option<&str>,
) -> Result<Arc<AppState>, Box<dyn std::error::Error>> {
    init_state_with_config(manager, model_alias, ServerConfig::default()).await
}

/// Create state with custom configuration.
pub async fn init_state_with_config(
    manager: &'static FoundryLocalManager,
    model_alias: Option<&str>,
    config: ServerConfig,
) -> Result<Arc<AppState>, Box<dyn std::error::Error>> {
    // Download execution providers
    manager.download_and_register_eps(None).await?;

    // Optionally load a specific model
    if let Some(alias) = model_alias {
        let model = manager.catalog().get_model(alias).await?;
        if !model.is_cached().await? {
            model.download(None::<fn(f64)>).await?;
        }
        model.load().await?;
    }

    let agents = AppState::build_agents(manager).await;

    // Initialise persistence
    let db = if let Some(path) = &config.db_path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        Some(persistence::Db::open(path)?)
    } else {
        None
    };

    // Initialise concurrency semaphore
    let run_semaphore = if config.max_concurrent_runs > 0 {
        Some(Arc::new(tokio::sync::Semaphore::new(
            config.max_concurrent_runs,
        )))
    } else {
        None
    };

    // Record start time for health endpoint
    health::record_start_time();

    Ok(Arc::new(AppState {
        agents,
        threads: DashMap::new(),
        runs: DashMap::new(),
        run_streams: DashMap::new(),
        manager,
        max_runs: config.max_concurrent_runs,
        run_timeout: config.run_timeout,
        run_semaphore,
        db,
    }))
}
