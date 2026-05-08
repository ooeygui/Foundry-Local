// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Agent route handlers: search, get by ID, get descriptor.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::Json;

use crate::acp_types::*;
use crate::error::AcpError;
use crate::state::AppState;

/// POST /agents/search
pub async fn search_agents(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AgentSearchRequest>,
) -> Result<Json<Vec<Agent>>, AcpError> {
    let mut results: Vec<Agent> = state
        .agents
        .values()
        .filter(|entry| {
            if let Some(ref name) = req.name {
                if !entry.agent.metadata.agent_ref.name.contains(name.as_str()) {
                    return false;
                }
            }
            if let Some(ref version) = req.version {
                if entry.agent.metadata.agent_ref.version != *version {
                    return false;
                }
            }
            true
        })
        .map(|entry| entry.agent.clone())
        .collect();

    // Sort for deterministic results
    results.sort_by(|a, b| {
        a.metadata
            .agent_ref
            .name
            .cmp(&b.metadata.agent_ref.name)
    });

    let offset = req.offset.max(0) as usize;
    let limit = req.limit.max(1) as usize;

    let page: Vec<Agent> = results.into_iter().skip(offset).take(limit).collect();
    Ok(Json(page))
}

/// GET /agents/:agent_id
pub async fn get_agent(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<uuid::Uuid>,
) -> Result<Json<Agent>, AcpError> {
    let entry = state
        .agents
        .get(&agent_id)
        .ok_or_else(|| AcpError::not_found(format!("Agent not found: {agent_id}")))?;
    Ok(Json(entry.agent.clone()))
}

/// GET /agents/:agent_id/descriptor
pub async fn get_agent_descriptor(
    State(state): State<Arc<AppState>>,
    Path(agent_id): Path<uuid::Uuid>,
) -> Result<Json<AgentACPDescriptor>, AcpError> {
    let entry = state
        .agents
        .get(&agent_id)
        .ok_or_else(|| AcpError::not_found(format!("Agent not found: {agent_id}")))?;
    Ok(Json(entry.descriptor.clone()))
}
