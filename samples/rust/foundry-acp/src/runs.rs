// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Stateless run route handlers.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::http::StatusCode;
use axum::Json;
use chrono::Utc;
use futures_core::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::acp_types::*;
use crate::error::AcpError;
use crate::inference;
use crate::state::AppState;

/// Persist to SQLite (best-effort; logs errors but never fails the request).
macro_rules! persist {
    ($expr:expr) => {
        if let Err(e) = $expr {
            tracing::warn!("Persistence error: {e}");
        }
    };
}

/// POST /runs — create a background stateless run.
pub async fn create_stateless_run(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunCreateStateless>,
) -> Result<Json<RunStateless>, AcpError> {
    let agent_id = state.resolve_agent_id(req.base.agent_id.as_deref())?;
    let run_id = state.create_run(agent_id, None);

    // Persist run creation
    if let Some(ref db) = state.db {
        let input_val = req.base.input.as_ref();
        let config_val = req.base.config.as_ref().map(|c| serde_json::to_value(c).unwrap_or_default());
        persist!(db.insert_run(&run_id.to_string(), &agent_id.to_string(), None, input_val, config_val.as_ref(), &Utc::now()).await);
    }

    // Store creation payload
    {
        let run_data = state.runs.get(&run_id).unwrap();
        let mut rd = run_data.lock().await;
        rd.creation_stateless = Some(req.clone());
    }

    // Spawn background execution
    let state2 = Arc::clone(&state);
    let input = req.base.input.clone();
    let config = req.base.config.clone();
    tokio::spawn(async move {
        inference::execute_run(&state2, run_id, agent_id, input, config, None).await;
    });

    let run_data = state.runs.get(&run_id).unwrap();
    let rd = run_data.lock().await;
    Ok(Json(rd.to_stateless()))
}

/// POST /runs/wait — create a stateless run and wait for result.
pub async fn create_and_wait_stateless(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunCreateStateless>,
) -> Result<Json<RunWaitResponseStateless>, AcpError> {
    let agent_id = state.resolve_agent_id(req.base.agent_id.as_deref())?;
    let run_id = state.create_run(agent_id, None);

    {
        let run_data = state.runs.get(&run_id).unwrap();
        let mut rd = run_data.lock().await;
        rd.creation_stateless = Some(req.clone());
    }

    // Execute inline (blocking for the caller)
    inference::execute_run(&state, run_id, agent_id, req.base.input, req.base.config, None).await;

    let run_data = state.runs.get(&run_id).unwrap();
    let rd = run_data.lock().await;
    Ok(Json(RunWaitResponseStateless {
        run: Some(rd.to_stateless()),
        output: rd.output.clone(),
    }))
}

/// POST /runs/stream — create a stateless run and stream output.
pub async fn create_and_stream_stateless(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunCreateStateless>,
) -> Result<impl IntoResponse, AcpError> {
    let agent_id = state.resolve_agent_id(req.base.agent_id.as_deref())?;
    let run_id = state.create_run(agent_id, None);

    {
        let run_data = state.runs.get(&run_id).unwrap();
        let mut rd = run_data.lock().await;
        rd.creation_stateless = Some(req.clone());
    }

    // Subscribe before spawning so we don't miss events
    let rx = state
        .run_streams
        .get(&run_id)
        .ok_or_else(|| AcpError::internal("Stream channel not found"))?
        .subscribe();

    let state2 = Arc::clone(&state);
    let input = req.base.input.clone();
    let config = req.base.config.clone();
    tokio::spawn(async move {
        inference::execute_streaming_run(&state2, run_id, agent_id, input, config, None).await;
    });

    let stream = make_sse_stream(rx);
    Ok(Sse::new(stream))
}

/// POST /runs/search
pub async fn search_stateless_runs(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunSearchRequest>,
) -> Result<Json<Vec<RunStateless>>, AcpError> {
    let mut results = Vec::new();
    for entry in state.runs.iter() {
        let rd = entry.lock().await;
        // Only include stateless runs (no thread_id)
        if rd.run.thread_id.is_some() {
            continue;
        }
        if let Some(ref agent_id) = req.agent_id {
            if rd.run.agent_id != *agent_id {
                continue;
            }
        }
        if let Some(ref status) = req.status {
            if rd.run.status != *status {
                continue;
            }
        }
        results.push(rd.to_stateless());
    }
    let offset = req.offset.max(0) as usize;
    let limit = req.limit.max(1) as usize;
    let page: Vec<_> = results.into_iter().skip(offset).take(limit).collect();
    Ok(Json(page))
}

/// GET /runs/:run_id
pub async fn get_stateless_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<RunStateless>, AcpError> {
    let entry = state
        .runs
        .get(&run_id)
        .ok_or_else(|| AcpError::not_found(format!("Run not found: {run_id}")))?;
    let rd = entry.lock().await;
    Ok(Json(rd.to_stateless()))
}

/// GET /runs/:run_id/wait
pub async fn wait_stateless_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<Uuid>,
) -> Result<Json<RunWaitResponseStateless>, AcpError> {
    // Poll until run is no longer pending
    loop {
        let entry = state
            .runs
            .get(&run_id)
            .ok_or_else(|| AcpError::not_found(format!("Run not found: {run_id}")))?;
        let rd = entry.lock().await;
        if rd.run.status != RunStatus::Pending {
            return Ok(Json(RunWaitResponseStateless {
                run: Some(rd.to_stateless()),
                output: rd.output.clone(),
            }));
        }
        drop(rd);
        drop(entry);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// GET /runs/:run_id/stream
pub async fn stream_stateless_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<Uuid>,
) -> Result<impl IntoResponse, AcpError> {
    let rx = state
        .run_streams
        .get(&run_id)
        .ok_or_else(|| AcpError::not_found(format!("Run not found: {run_id}")))?
        .subscribe();

    let stream = make_sse_stream(rx);
    Ok(Sse::new(stream))
}

/// POST /runs/:run_id (resume) — not supported
pub async fn resume_stateless_run(
    Path(_run_id): Path<Uuid>,
) -> impl IntoResponse {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json("Interrupts and resume are not supported by this server"),
    )
}

/// DELETE /runs/:run_id
pub async fn delete_stateless_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<Uuid>,
) -> Result<StatusCode, AcpError> {
    state
        .runs
        .remove(&run_id)
        .ok_or_else(|| AcpError::not_found(format!("Run not found: {run_id}")))?;
    state.run_streams.remove(&run_id);

    // Persist deletion
    if let Some(ref db) = state.db {
        persist!(db.delete_run(&run_id.to_string()).await);
    }

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, serde::Deserialize)]
pub struct CancelQuery {
    #[serde(default)]
    pub wait: bool,
    #[serde(default = "default_cancel_action")]
    pub action: CancelAction,
}

fn default_cancel_action() -> CancelAction {
    CancelAction::Interrupt
}

/// POST /runs/:run_id/cancel
pub async fn cancel_stateless_run(
    State(state): State<Arc<AppState>>,
    Path(run_id): Path<Uuid>,
    Query(_query): Query<CancelQuery>,
) -> Result<StatusCode, AcpError> {
    let entry = state
        .runs
        .get(&run_id)
        .ok_or_else(|| AcpError::not_found(format!("Run not found: {run_id}")))?;
    let mut rd = entry.lock().await;
    if rd.run.status == RunStatus::Pending {
        rd.run.status = RunStatus::Error;
        rd.run.updated_at = Utc::now();
        rd.output = Some(RunOutput::Error {
            run_id,
            errcode: 499,
            description: "Run cancelled by client".into(),
        });
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── SSE helper ───────────────────────────────────────────────────────────────

fn make_sse_stream(
    rx: tokio::sync::broadcast::Receiver<ValueRunResultUpdate>,
) -> impl Stream<Item = Result<Event, std::convert::Infallible>> {
    BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(update) => {
            let data = serde_json::to_string(&update).unwrap_or_default();
            Some(Ok(Event::default().event("agent_event").data(data)))
        }
        Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(_)) => None,
    })
}
