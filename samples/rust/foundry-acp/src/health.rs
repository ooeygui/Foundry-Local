// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Health and readiness probe endpoint.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::state::AppState;

/// Server health response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub acp_spec_version: &'static str,
    pub uptime_seconds: u64,
    pub agents_registered: usize,
    pub threads_active: usize,
    pub runs_active: usize,
    pub models: Vec<ModelHealth>,
}

#[derive(Debug, Serialize)]
pub struct ModelHealth {
    pub alias: String,
    pub agent_id: String,
    pub loaded: bool,
}

/// Global start time for uptime calculation.
static START_TIME: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

pub fn record_start_time() {
    START_TIME.get_or_init(Instant::now);
}

/// GET /health
pub async fn health_check(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let uptime = START_TIME
        .get()
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);

    let models: Vec<ModelHealth> = state
        .agents
        .iter()
        .map(|(id, entry)| ModelHealth {
            alias: entry.model_alias.clone(),
            agent_id: id.to_string(),
            loaded: entry.is_loaded,
        })
        .collect();

    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        acp_spec_version: "0.2.3",
        uptime_seconds: uptime,
        agents_registered: state.agents.len(),
        threads_active: state.threads.len(),
        runs_active: state.runs.len(),
        models,
    })
}
