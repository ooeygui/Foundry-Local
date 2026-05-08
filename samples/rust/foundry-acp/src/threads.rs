// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Thread and thread-run route handlers.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::Json;
use chrono::Utc;
use tokio::sync::Mutex;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use uuid::Uuid;

use crate::acp_types::*;
use crate::error::AcpError;
use crate::inference;
use crate::state::{AppState, ThreadData};

/// Persist to SQLite (best-effort; logs errors but never fails the request).
macro_rules! persist {
    ($expr:expr) => {
        if let Err(e) = $expr {
            tracing::warn!("Persistence error: {e}");
        }
    };
}

// ── Thread CRUD ──────────────────────────────────────────────────────────────

/// POST /threads
pub async fn create_thread(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ThreadCreate>,
) -> Result<Json<Thread>, AcpError> {
    let thread_id = req
        .thread_id
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    if state.threads.contains_key(&thread_id) {
        match req.if_exists {
            IfExists::DoNothing => {
                let entry = state.threads.get(&thread_id).unwrap();
                let td = entry.lock().await;
                return Ok(Json(td.thread.clone()));
            }
            IfExists::Raise => {
                return Err(AcpError::conflict(format!(
                    "Thread already exists: {thread_id}"
                )));
            }
        }
    }

    let now = Utc::now();
    let thread = Thread {
        thread_id: thread_id.clone(),
        created_at: now,
        updated_at: now,
        metadata: req.metadata.unwrap_or(serde_json::json!({})),
        status: ThreadStatus::Idle,
        values: None,
        messages: Some(vec![]),
    };

    let td = ThreadData {
        thread: thread.clone(),
        messages: vec![],
        history: vec![],
    };

    state
        .threads
        .insert(thread_id.clone(), Arc::new(Mutex::new(td)));

    // Persist to SQLite
    if let Some(ref db) = state.db {
        persist!(db.insert_thread(&thread_id, &thread.metadata, &now).await);
    }

    Ok(Json(thread))
}

/// POST /threads/search
pub async fn search_threads(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ThreadSearchRequest>,
) -> Result<Json<Vec<Thread>>, AcpError> {
    let mut results = Vec::new();
    for entry in state.threads.iter() {
        let td = entry.lock().await;
        if let Some(ref status) = req.status {
            if td.thread.status != *status {
                continue;
            }
        }
        results.push(td.thread.clone());
    }
    let offset = req.offset.max(0) as usize;
    let limit = req.limit.max(1) as usize;
    let page: Vec<_> = results.into_iter().skip(offset).take(limit).collect();
    Ok(Json(page))
}

/// GET /threads/:thread_id
pub async fn get_thread(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
) -> Result<Json<Thread>, AcpError> {
    let entry = state
        .threads
        .get(&thread_id)
        .ok_or_else(|| AcpError::not_found(format!("Thread not found: {thread_id}")))?;
    let td = entry.lock().await;
    Ok(Json(td.thread.clone()))
}

/// PATCH /threads/:thread_id
pub async fn patch_thread(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
    Json(req): Json<ThreadPatch>,
) -> Result<Json<Thread>, AcpError> {
    let entry = state
        .threads
        .get(&thread_id)
        .ok_or_else(|| AcpError::not_found(format!("Thread not found: {thread_id}")))?;
    let mut td = entry.lock().await;

    if let Some(metadata) = req.metadata {
        if let (serde_json::Value::Object(existing), serde_json::Value::Object(patch)) =
            (&mut td.thread.metadata, metadata)
        {
            for (k, v) in patch {
                existing.insert(k, v);
            }
        }
    }
    if let Some(values) = req.values {
        td.thread.values = Some(values);
    }
    if let Some(messages) = req.messages {
        td.messages = messages.clone();
        td.thread.messages = Some(messages);
    }
    td.thread.updated_at = Utc::now();

    // Persist changes
    if let Some(ref db) = state.db {
        persist!(db.update_thread_metadata(&thread_id, &td.thread.metadata, &td.thread.updated_at).await);
        if td.thread.messages.is_some() {
            persist!(db.append_messages(&thread_id, &td.messages, &td.thread.updated_at).await);
        }
    }

    Ok(Json(td.thread.clone()))
}

/// DELETE /threads/:thread_id
pub async fn delete_thread(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
) -> Result<StatusCode, AcpError> {
    // Check for pending runs
    let entry = state
        .threads
        .get(&thread_id)
        .ok_or_else(|| AcpError::not_found(format!("Thread not found: {thread_id}")))?;
    let td = entry.lock().await;
    if td.thread.status == ThreadStatus::Busy {
        return Err(AcpError::conflict(
            "Cannot delete thread with pending runs",
        ));
    }
    drop(td);
    drop(entry);

    state.threads.remove(&thread_id);

    // Persist deletion
    if let Some(ref db) = state.db {
        persist!(db.delete_thread(&thread_id).await);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// POST /threads/:thread_id/copy
pub async fn copy_thread(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
) -> Result<Json<Thread>, AcpError> {
    let entry = state
        .threads
        .get(&thread_id)
        .ok_or_else(|| AcpError::not_found(format!("Thread not found: {thread_id}")))?;
    let td = entry.lock().await;

    let new_id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let new_thread = Thread {
        thread_id: new_id.clone(),
        created_at: now,
        updated_at: now,
        metadata: td.thread.metadata.clone(),
        status: ThreadStatus::Idle,
        values: td.thread.values.clone(),
        messages: Some(td.messages.clone()),
    };

    let new_td = ThreadData {
        thread: new_thread.clone(),
        messages: td.messages.clone(),
        history: td.history.clone(),
    };
    drop(td);
    drop(entry);

    state
        .threads
        .insert(new_id.clone(), Arc::new(Mutex::new(new_td)));

    // Persist the copy
    if let Some(ref db) = state.db {
        persist!(db.insert_thread(&new_id, &new_thread.metadata, &now).await);
    }

    Ok(Json(new_thread))
}

#[derive(Debug, serde::Deserialize)]
pub struct HistoryQuery {
    #[serde(default = "default_history_limit")]
    pub limit: i64,
    pub before: Option<String>,
}

fn default_history_limit() -> i64 {
    10
}

/// GET /threads/:thread_id/history
pub async fn get_thread_history(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Vec<ThreadState>>, AcpError> {
    let entry = state
        .threads
        .get(&thread_id)
        .ok_or_else(|| AcpError::not_found(format!("Thread not found: {thread_id}")))?;
    let td = entry.lock().await;
    let limit = query.limit.max(1) as usize;
    let history: Vec<_> = td.history.iter().rev().take(limit).cloned().collect();
    Ok(Json(history))
}

// ── Thread Runs ──────────────────────────────────────────────────────────────

/// GET /threads/:thread_id/runs
pub async fn list_thread_runs(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
    Query(pagination): Query<PaginationQuery>,
) -> Result<Json<Vec<RunStateful>>, AcpError> {
    // Verify thread exists
    if !state.threads.contains_key(&thread_id) {
        return Err(AcpError::not_found(format!("Thread not found: {thread_id}")));
    }

    let thread_uuid = thread_id
        .parse::<Uuid>()
        .unwrap_or_else(|_| Uuid::new_v5(&Uuid::NAMESPACE_URL, thread_id.as_bytes()));

    let mut results = Vec::new();
    for entry in state.runs.iter() {
        let rd = entry.lock().await;
        if rd.run.thread_id == Some(thread_uuid) {
            results.push(rd.to_stateful());
        }
    }

    let offset = pagination.offset.unwrap_or(0).max(0) as usize;
    let limit = pagination.limit.unwrap_or(10).max(1) as usize;
    let page: Vec<_> = results.into_iter().skip(offset).take(limit).collect();
    Ok(Json(page))
}

#[derive(Debug, serde::Deserialize)]
pub struct PaginationQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// POST /threads/:thread_id/runs — create a background thread run
pub async fn create_thread_run(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
    Json(req): Json<RunCreateStateful>,
) -> Result<Json<RunStateful>, AcpError> {
    let (run_id, agent_id) = prepare_thread_run(&state, &thread_id, &req).await?;

    let state2 = Arc::clone(&state);
    let input = req.base.input.clone();
    let config = req.base.config.clone();
    let tid = thread_id.clone();
    tokio::spawn(async move {
        run_on_thread(&state2, run_id, agent_id, &tid, input, config).await;
    });

    let entry = state.runs.get(&run_id).unwrap();
    let rd = entry.lock().await;
    Ok(Json(rd.to_stateful()))
}

/// POST /threads/:thread_id/runs/wait
pub async fn create_and_wait_thread_run(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
    Json(req): Json<RunCreateStateful>,
) -> Result<Json<RunWaitResponseStateful>, AcpError> {
    let (run_id, agent_id) = prepare_thread_run(&state, &thread_id, &req).await?;

    run_on_thread(&state, run_id, agent_id, &thread_id, req.base.input, req.base.config).await;

    let entry = state.runs.get(&run_id).unwrap();
    let rd = entry.lock().await;
    Ok(Json(RunWaitResponseStateful {
        run: Some(rd.to_stateful()),
        output: rd.output.clone(),
    }))
}

/// POST /threads/:thread_id/runs/stream
pub async fn create_and_stream_thread_run(
    State(state): State<Arc<AppState>>,
    Path(thread_id): Path<String>,
    Json(req): Json<RunCreateStateful>,
) -> Result<impl IntoResponse, AcpError> {
    let (run_id, agent_id) = prepare_thread_run(&state, &thread_id, &req).await?;

    let rx = state
        .run_streams
        .get(&run_id)
        .ok_or_else(|| AcpError::internal("Stream channel not found"))?
        .subscribe();

    let state2 = Arc::clone(&state);
    let input = req.base.input.clone();
    let config = req.base.config.clone();
    let tid = thread_id.clone();
    tokio::spawn(async move {
        run_on_thread_streaming(&state2, run_id, agent_id, &tid, input, config).await;
    });

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(update) => {
            let data = serde_json::to_string(&update).unwrap_or_default();
            Some(Ok::<_, std::convert::Infallible>(
                Event::default().event("agent_event").data(data),
            ))
        }
        Err(_) => None,
    });

    Ok(Sse::new(stream))
}

/// GET /threads/:thread_id/runs/:run_id
pub async fn get_thread_run(
    State(state): State<Arc<AppState>>,
    Path((thread_id, run_id)): Path<(String, Uuid)>,
) -> Result<Json<RunStateful>, AcpError> {
    let _ = &thread_id; // Thread context validated by run's thread_id
    let entry = state
        .runs
        .get(&run_id)
        .ok_or_else(|| AcpError::not_found(format!("Run not found: {run_id}")))?;
    let rd = entry.lock().await;
    Ok(Json(rd.to_stateful()))
}

/// GET /threads/:thread_id/runs/:run_id/wait
pub async fn wait_thread_run(
    State(state): State<Arc<AppState>>,
    Path((_thread_id, run_id)): Path<(String, Uuid)>,
) -> Result<Json<RunWaitResponseStateful>, AcpError> {
    loop {
        let entry = state
            .runs
            .get(&run_id)
            .ok_or_else(|| AcpError::not_found(format!("Run not found: {run_id}")))?;
        let rd = entry.lock().await;
        if rd.run.status != RunStatus::Pending {
            return Ok(Json(RunWaitResponseStateful {
                run: Some(rd.to_stateful()),
                output: rd.output.clone(),
            }));
        }
        drop(rd);
        drop(entry);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// GET /threads/:thread_id/runs/:run_id/stream
pub async fn stream_thread_run(
    State(state): State<Arc<AppState>>,
    Path((_thread_id, run_id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, AcpError> {
    let rx = state
        .run_streams
        .get(&run_id)
        .ok_or_else(|| AcpError::not_found(format!("Run not found: {run_id}")))?
        .subscribe();

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(update) => {
            let data = serde_json::to_string(&update).unwrap_or_default();
            Some(Ok::<_, std::convert::Infallible>(
                Event::default().event("agent_event").data(data),
            ))
        }
        Err(_) => None,
    });

    Ok(Sse::new(stream))
}

/// POST /threads/:thread_id/runs/:run_id (resume) — not supported
pub async fn resume_thread_run(
    Path((_thread_id, _run_id)): Path<(String, Uuid)>,
) -> impl IntoResponse {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json("Interrupts and resume are not supported"),
    )
}

/// DELETE /threads/:thread_id/runs/:run_id
pub async fn delete_thread_run(
    State(state): State<Arc<AppState>>,
    Path((_thread_id, run_id)): Path<(String, Uuid)>,
) -> Result<StatusCode, AcpError> {
    state
        .runs
        .remove(&run_id)
        .ok_or_else(|| AcpError::not_found(format!("Run not found: {run_id}")))?;
    state.run_streams.remove(&run_id);
    Ok(StatusCode::NO_CONTENT)
}

/// POST /threads/:thread_id/runs/:run_id/cancel
pub async fn cancel_thread_run(
    State(state): State<Arc<AppState>>,
    Path((_thread_id, run_id)): Path<(String, Uuid)>,
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
            description: "Run cancelled".into(),
        });
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

async fn prepare_thread_run(
    state: &AppState,
    thread_id: &str,
    req: &RunCreateStateful,
) -> Result<(Uuid, Uuid), AcpError> {
    // Ensure thread exists (or create if if_not_exists == "create")
    if !state.threads.contains_key(thread_id) {
        match req.if_not_exists {
            IfNotExists::Create => {
                let now = Utc::now();
                let thread = Thread {
                    thread_id: thread_id.to_string(),
                    created_at: now,
                    updated_at: now,
                    metadata: serde_json::json!({}),
                    status: ThreadStatus::Idle,
                    values: None,
                    messages: Some(vec![]),
                };
                let td = ThreadData {
                    thread,
                    messages: vec![],
                    history: vec![],
                };
                state
                    .threads
                    .insert(thread_id.to_string(), Arc::new(Mutex::new(td)));
                // Persist auto-created thread
                if let Some(ref db) = state.db {
                    persist!(db.insert_thread(thread_id, &serde_json::json!({}), &now).await);
                }
            }
            IfNotExists::Reject => {
                return Err(AcpError::not_found(format!(
                    "Thread not found: {thread_id}"
                )));
            }
        }
    }

    // Check thread is idle
    {
        let entry = state.threads.get(thread_id).unwrap();
        let mut td = entry.lock().await;
        if td.thread.status == ThreadStatus::Busy {
            return Err(AcpError::conflict("Thread already has a pending run"));
        }
        td.thread.status = ThreadStatus::Busy;
        td.thread.updated_at = Utc::now();
        // Persist status change
        if let Some(ref db) = state.db {
            persist!(db.update_thread_status(thread_id, ThreadStatus::Busy, &td.thread.updated_at).await);
        }
    }

    let agent_id = state.resolve_agent_id(req.base.agent_id.as_deref())?;
    let thread_uuid = thread_id
        .parse::<Uuid>()
        .unwrap_or_else(|_| Uuid::new_v5(&Uuid::NAMESPACE_URL, thread_id.as_bytes()));
    let run_id = state.create_run(agent_id, Some(thread_uuid));

    // Store creation payload
    {
        let run_data = state.runs.get(&run_id).unwrap();
        let mut rd = run_data.lock().await;
        rd.creation_stateful = Some(req.clone());
    }

    Ok((run_id, agent_id))
}

async fn run_on_thread(
    state: &AppState,
    run_id: Uuid,
    agent_id: Uuid,
    thread_id: &str,
    input: Option<serde_json::Value>,
    config: Option<RunConfig>,
) {
    // Get thread messages and add input messages to thread
    let thread_messages = {
        let entry = match state.threads.get(thread_id) {
            Some(e) => e,
            None => return,
        };
        let mut td = entry.lock().await;

        // Add input messages to thread history
        if let Some(ref input_val) = input {
            if let Some(msgs) = input_val.get("messages").and_then(|m| m.as_array()) {
                for m in msgs {
                    if let (Some(role), Some(content)) =
                        (m.get("role").and_then(|r| r.as_str()), m.get("content"))
                    {
                        td.messages.push(Message {
                            role: role.to_string(),
                            content: content.clone(),
                            id: None,
                            metadata: None,
                        });
                    }
                }
            }
        }
        td.messages.clone()
    };

    inference::execute_run(state, run_id, agent_id, input, config, Some(thread_messages)).await;

    // Update thread with response and set back to idle
    if let Some(entry) = state.threads.get(thread_id) {
        let mut td = entry.lock().await;

        // Add assistant response to thread messages
        if let Some(run_entry) = state.runs.get(&run_id) {
            let rd = run_entry.lock().await;
            if let Some(RunOutput::Result { messages, .. }) = &rd.output {
                if let Some(msgs) = messages {
                    td.messages.extend(msgs.clone());
                }
            }
        }

        td.thread.messages = Some(td.messages.clone());
        td.thread.status = ThreadStatus::Idle;
        td.thread.updated_at = Utc::now();

        // Persist thread state
        if let Some(ref db) = state.db {
            persist!(db.update_thread_status(thread_id, ThreadStatus::Idle, &td.thread.updated_at).await);
            persist!(db.append_messages(thread_id, &td.messages, &td.thread.updated_at).await);
        }

        // Create checkpoint
        let checkpoint = ThreadState {
            checkpoint: ThreadCheckpoint {
                checkpoint_id: Uuid::new_v4(),
            },
            values: serde_json::json!({ "messages": td.messages }),
            messages: Some(td.messages.clone()),
            metadata: None,
        };
        td.history.push(checkpoint);
    }
}

async fn run_on_thread_streaming(
    state: &AppState,
    run_id: Uuid,
    agent_id: Uuid,
    thread_id: &str,
    input: Option<serde_json::Value>,
    config: Option<RunConfig>,
) {
    let thread_messages = {
        let entry = match state.threads.get(thread_id) {
            Some(e) => e,
            None => return,
        };
        let mut td = entry.lock().await;

        if let Some(ref input_val) = input {
            if let Some(msgs) = input_val.get("messages").and_then(|m| m.as_array()) {
                for m in msgs {
                    if let (Some(role), Some(content)) =
                        (m.get("role").and_then(|r| r.as_str()), m.get("content"))
                    {
                        td.messages.push(Message {
                            role: role.to_string(),
                            content: content.clone(),
                            id: None,
                            metadata: None,
                        });
                    }
                }
            }
        }
        td.messages.clone()
    };

    inference::execute_streaming_run(state, run_id, agent_id, input, config, Some(thread_messages))
        .await;

    // Update thread
    if let Some(entry) = state.threads.get(thread_id) {
        let mut td = entry.lock().await;

        if let Some(run_entry) = state.runs.get(&run_id) {
            let rd = run_entry.lock().await;
            if let Some(RunOutput::Result { messages, .. }) = &rd.output {
                if let Some(msgs) = messages {
                    td.messages.extend(msgs.clone());
                }
            }
        }

        td.thread.messages = Some(td.messages.clone());
        td.thread.status = ThreadStatus::Idle;
        td.thread.updated_at = Utc::now();

        // Persist thread state
        if let Some(ref db) = state.db {
            persist!(db.update_thread_status(thread_id, ThreadStatus::Idle, &td.thread.updated_at).await);
            persist!(db.append_messages(thread_id, &td.messages, &td.thread.updated_at).await);
        }

        let checkpoint = ThreadState {
            checkpoint: ThreadCheckpoint {
                checkpoint_id: Uuid::new_v4(),
            },
            values: serde_json::json!({ "messages": td.messages }),
            messages: Some(td.messages.clone()),
            metadata: None,
        };
        td.history.push(checkpoint);
    }
}
