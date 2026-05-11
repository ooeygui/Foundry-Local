// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Application state shared across all ACP route handlers.
//!
//! Manages agent registry (backed by Foundry catalog), thread store, and run store.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;
use tokio::sync::{broadcast, Mutex};
use uuid::Uuid;

use crate::acp_types::*;

/// Namespace UUID for generating deterministic agent IDs from model aliases.
const AGENT_NS: Uuid = Uuid::from_bytes([
    0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
    0xc8,
]);

/// Deterministic agent ID from a model alias.
pub fn agent_id_from_alias(alias: &str) -> Uuid {
    Uuid::new_v5(&AGENT_NS, alias.as_bytes())
}

/// Internal representation of a registered ACP agent backed by a Foundry model.
#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub agent: Agent,
    pub descriptor: AgentACPDescriptor,
    pub model_alias: String,
    /// Whether the model was loaded at registration time.
    pub is_loaded: bool,
}

/// Internal thread data protected by a per-thread mutex.
#[derive(Debug, Clone)]
pub struct ThreadData {
    pub thread: Thread,
    pub messages: Vec<Message>,
    pub history: Vec<ThreadState>,
}

/// Internal run data.
#[derive(Debug, Clone)]
pub struct RunData {
    pub run: Run,
    pub creation_stateless: Option<RunCreateStateless>,
    pub creation_stateful: Option<RunCreateStateful>,
    pub output: Option<RunOutput>,
}

impl RunData {
    pub fn to_stateless(&self) -> RunStateless {
        RunStateless {
            run: self.run.clone(),
            creation: self.creation_stateless.clone().unwrap_or_else(|| {
                RunCreateStateless {
                    base: RunCreate {
                        agent_id: None,
                        input: None,
                        metadata: None,
                        config: None,
                        webhook: None,
                        stream_mode: None,
                        on_disconnect: OnDisconnect::Cancel,
                        multitask_strategy: MultitaskStrategy::Reject,
                        after_seconds: None,
                    },
                    on_completion: OnCompletion::Delete,
                }
            }),
        }
    }

    pub fn to_stateful(&self) -> RunStateful {
        RunStateful {
            run: self.run.clone(),
            creation: self.creation_stateful.clone().unwrap_or_else(|| {
                RunCreateStateful {
                    base: RunCreate {
                        agent_id: None,
                        input: None,
                        metadata: None,
                        config: None,
                        webhook: None,
                        stream_mode: None,
                        on_disconnect: OnDisconnect::Cancel,
                        multitask_strategy: MultitaskStrategy::Reject,
                        after_seconds: None,
                    },
                    stream_subgraphs: false,
                    if_not_exists: IfNotExists::Reject,
                }
            }),
        }
    }
}

/// Per-run broadcast channel for streaming output.
pub type StreamSender = broadcast::Sender<ValueRunResultUpdate>;

/// Shared application state.
pub struct AppState {
    pub agents: HashMap<Uuid, AgentEntry>,
    pub threads: DashMap<String, Arc<Mutex<ThreadData>>>,
    pub runs: DashMap<Uuid, Arc<Mutex<RunData>>>,
    /// Broadcast channels for SSE streaming, keyed by run_id.
    pub run_streams: DashMap<Uuid, StreamSender>,
    /// Foundry Local manager reference (static lifetime via OnceLock singleton).
    pub manager: &'static foundry_local_sdk::FoundryLocalManager,
    /// Maximum concurrent runs (0 = unlimited).
    pub max_runs: usize,
    /// Maximum time a single run may execute.
    pub run_timeout: std::time::Duration,
    /// Semaphore for concurrent run limiting. None = unlimited.
    pub run_semaphore: Option<Arc<tokio::sync::Semaphore>>,
    /// Optional SQLite persistence layer.
    pub db: Option<Arc<crate::persistence::Db>>,
}

impl AppState {
    /// Build the agent registry from the current Foundry model catalog.
    pub async fn build_agents(
        manager: &'static foundry_local_sdk::FoundryLocalManager,
    ) -> HashMap<Uuid, AgentEntry> {
        let mut agents = HashMap::new();
        let models = match manager.catalog().get_models().await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("Failed to get models: {e}");
                return agents;
            }
        };

        for model in &models {
            let alias = model.alias().to_string();
            let entry = Self::make_agent_entry(&model, &alias).await;
            agents.insert(entry.agent.agent_id, entry);
        }

        agents
    }

    /// Build a single-agent registry for just one model alias.
    /// Avoids the full catalog scan that can trigger ORT API version warnings.
    pub async fn build_agent_single(
        manager: &'static foundry_local_sdk::FoundryLocalManager,
        alias: &str,
    ) -> HashMap<Uuid, AgentEntry> {
        let mut agents = HashMap::new();
        match manager.catalog().get_model(alias).await {
            Ok(model) => {
                let entry = Self::make_agent_entry(&model, alias).await;
                agents.insert(entry.agent.agent_id, entry);
            }
            Err(e) => {
                tracing::warn!("Failed to get model '{alias}': {e}");
            }
        }
        agents
    }

    /// Create an AgentEntry from a Foundry model.
    async fn make_agent_entry(
        model: &foundry_local_sdk::Model,
        alias: &str,
    ) -> AgentEntry {
        let id = agent_id_from_alias(alias);
        let info = model.info();

        let description = format!(
            "Foundry Local model '{}' ({}) — {}",
            alias,
            info.model_type,
            info.display_name
                .as_deref()
                .unwrap_or(&info.name)
        );

        let metadata = AgentMetadata {
            agent_ref: AgentRef {
                name: alias.to_string(),
                version: info.version.to_string(),
                url: None,
            },
            description: description.clone(),
        };

        let input_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "messages": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "role": { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["role", "content"]
                    }
                }
            },
            "required": ["messages"]
        });

        let output_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "messages": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "role": { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["role", "content"]
                    }
                }
            }
        });

        let config_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "temperature": { "type": "number", "minimum": 0.0, "maximum": 2.0 },
                "max_tokens": { "type": "integer", "minimum": 1 },
                "top_p": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
            }
        });

        let thread_state_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "messages": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "role": { "type": "string" },
                            "content": { "type": "string" }
                        }
                    }
                }
            }
        });

        let specs = AgentACPSpec {
            capabilities: AgentCapabilities {
                threads: true,
                interrupts: false,
                callbacks: false,
                streaming: Some(StreamingCapabilities {
                    values: Some(true),
                    custom: Some(false),
                }),
            },
            input: input_schema,
            output: output_schema,
            config: config_schema,
            thread_state: Some(thread_state_schema),
            interrupts: None,
            custom_streaming_update: None,
        };

        let is_loaded = model.is_loaded().await.unwrap_or(false);
        tracing::debug!("Discovered model '{}' (loaded={})", alias, is_loaded);

        AgentEntry {
            agent: Agent {
                agent_id: id,
                metadata: metadata.clone(),
            },
            descriptor: AgentACPDescriptor {
                metadata,
                specs,
            },
            model_alias: alias.to_string(),
            is_loaded,
        }
    }

    /// Create a new run record and return its ID.
    pub fn create_run(
        &self,
        agent_id: Uuid,
        thread_id: Option<Uuid>,
    ) -> Uuid {
        let run_id = Uuid::new_v4();
        let now = Utc::now();
        let run = Run {
            run_id,
            thread_id,
            agent_id,
            created_at: now,
            updated_at: now,
            status: RunStatus::Pending,
        };

        let data = RunData {
            run,
            creation_stateless: None,
            creation_stateful: None,
            output: None,
        };

        self.runs.insert(run_id, Arc::new(Mutex::new(data)));

        // Create broadcast channel for streaming
        let (tx, _) = broadcast::channel(256);
        self.run_streams.insert(run_id, tx);

        run_id
    }

    /// Resolve agent_id string from run creation payload.
    pub fn resolve_agent_id(&self, agent_id_str: Option<&str>) -> Result<Uuid, crate::error::AcpError> {
        match agent_id_str {
            Some(id_str) => {
                // Try parsing as UUID first
                if let Ok(uid) = id_str.parse::<Uuid>() {
                    if self.agents.contains_key(&uid) {
                        return Ok(uid);
                    }
                }
                // Try as model alias
                let uid = agent_id_from_alias(id_str);
                if self.agents.contains_key(&uid) {
                    Ok(uid)
                } else {
                    Err(crate::error::AcpError::not_found(format!(
                        "Agent not found: {id_str}"
                    )))
                }
            }
            None => {
                // Use the first (default) agent
                self.agents
                    .keys()
                    .next()
                    .copied()
                    .ok_or_else(|| crate::error::AcpError::not_found("No agents available"))
            }
        }
    }
}
