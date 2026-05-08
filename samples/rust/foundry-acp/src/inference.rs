// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Bridge between ACP runs and Foundry Local model inference.
//!
//! Converts ACP input (messages) into Foundry chat completions and writes
//! results back into the run store.

use chrono::Utc;
use uuid::Uuid;

use foundry_local_sdk::{
    ChatCompletionRequestMessage, ChatCompletionRequestUserMessage,
};

use crate::acp_types::*;
use crate::state::AppState;

/// Extract chat messages from ACP input JSON.
fn extract_messages(input: &Option<serde_json::Value>) -> Vec<ChatCompletionRequestMessage> {
    let Some(input) = input else {
        return vec![];
    };

    // Accept { "messages": [...] } or a plain string
    if let Some(text) = input.as_str() {
        return vec![ChatCompletionRequestMessage::User(
            ChatCompletionRequestUserMessage {
                content: text.to_string().into(),
                name: None,
            },
        )];
    }

    let msgs_val = input.get("messages").unwrap_or(input);
    let Some(arr) = msgs_val.as_array() else {
        // Try treating the entire input as a single user message
        if let Some(text) = input.get("content").and_then(|c| c.as_str()) {
            return vec![ChatCompletionRequestMessage::User(
                ChatCompletionRequestUserMessage {
                    content: text.to_string().into(),
                    name: None,
                },
            )];
        }
        return vec![];
    };

    arr.iter()
        .filter_map(|m| {
            let role = m.get("role")?.as_str()?;
            let content = m.get("content")?.as_str()?.to_string();
            match role {
                "user" => Some(ChatCompletionRequestMessage::User(
                    ChatCompletionRequestUserMessage {
                        content: content.into(),
                        name: None,
                    },
                )),
                "system" => Some(ChatCompletionRequestMessage::System(
                    foundry_local_sdk::ChatCompletionRequestSystemMessage {
                        content: content.into(),
                        name: None,
                    },
                )),
                "assistant" => Some(ChatCompletionRequestMessage::Assistant(
                    foundry_local_sdk::ChatCompletionRequestAssistantMessage {
                        content: Some(content.into()),
                        name: None,
                        tool_calls: None,
                        refusal: None,
                        audio: None,
                        function_call: None,
                    },
                )),
                _ => None,
            }
        })
        .collect()
}

/// Extract temperature/max_tokens/top_p from ACP config.
struct InferenceConfig {
    temperature: Option<f64>,
    max_tokens: Option<u32>,
    top_p: Option<f64>,
}

fn extract_config(config: &Option<RunConfig>) -> InferenceConfig {
    let Some(config) = config else {
        return InferenceConfig { temperature: None, max_tokens: None, top_p: None };
    };
    let configurable = config.configurable.as_ref();
    InferenceConfig {
        temperature: configurable.and_then(|c| c.get("temperature")).and_then(|v| v.as_f64()),
        max_tokens: configurable
            .and_then(|c| c.get("max_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        top_p: configurable.and_then(|c| c.get("top_p")).and_then(|v| v.as_f64()),
    }
}

/// Combine thread messages (if any) with input messages for the run.
fn build_message_list(
    thread_messages: Option<&[Message]>,
    input: &Option<serde_json::Value>,
) -> Vec<ChatCompletionRequestMessage> {
    let mut msgs = Vec::new();

    // Add thread history first
    if let Some(history) = thread_messages {
        for m in history {
            let content = match &m.content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let msg = match m.role.as_str() {
                "user" => ChatCompletionRequestMessage::User(
                    ChatCompletionRequestUserMessage {
                        content: content.into(),
                        name: None,
                    },
                ),
                "system" => ChatCompletionRequestMessage::System(
                    foundry_local_sdk::ChatCompletionRequestSystemMessage {
                        content: content.into(),
                        name: None,
                    },
                ),
                "assistant" => ChatCompletionRequestMessage::Assistant(
                    foundry_local_sdk::ChatCompletionRequestAssistantMessage {
                        content: Some(content.into()),
                        name: None,
                        tool_calls: None,
                        refusal: None,
                        audio: None,
                        function_call: None,
                    },
                ),
                _ => continue,
            };
            msgs.push(msg);
        }
    }

    // Add new input messages
    msgs.extend(extract_messages(input));
    msgs
}

/// Execute a non-streaming run (used by /runs, /runs/wait, thread runs).
///
/// Enforces run timeout and rate limiting via the semaphore in AppState.
pub async fn execute_run(
    state: &AppState,
    run_id: Uuid,
    agent_id: Uuid,
    input: Option<serde_json::Value>,
    config: Option<RunConfig>,
    thread_messages: Option<Vec<Message>>,
) {
    // Acquire concurrency semaphore
    let _permit = if let Some(ref sem) = state.run_semaphore {
        match sem.acquire().await {
            Ok(p) => Some(p),
            Err(_) => {
                update_run_error(state, run_id, "Server shutting down").await;
                return;
            }
        }
    } else {
        None
    };

    // Wrap execution with timeout
    let timeout_duration = state.run_timeout;
    let result = tokio::time::timeout(
        timeout_duration,
        execute_run_inner(state, agent_id, &input, &config, thread_messages.as_deref()),
    )
    .await;

    let entry = match state.runs.get(&run_id) {
        Some(e) => e,
        None => return,
    };
    let mut rd = entry.lock().await;

    match result {
        Ok(Ok((output_text, response_messages, usage))) => {
            rd.run.status = RunStatus::Success;
            rd.run.updated_at = Utc::now();
            rd.output = Some(RunOutput::Result {
                values: Some(serde_json::json!({
                    "text": output_text,
                    "usage": usage,
                })),
                messages: Some(response_messages),
            });

            // Send final update on broadcast channel
            if let Some(tx) = state.run_streams.get(&run_id) {
                let _ = tx.send(ValueRunResultUpdate {
                    update_type: "values".into(),
                    run_id,
                    status: RunStatus::Success,
                    values: serde_json::json!({ "text": output_text }),
                    messages: None,
                });
            }

            // Track metrics
            metrics::counter!("acp_runs_total", "status" => "success", "agent_id" => agent_id.to_string()).increment(1);
        }
        Ok(Err(err_msg)) => {
            rd.run.status = RunStatus::Error;
            rd.run.updated_at = Utc::now();
            rd.output = Some(RunOutput::Error {
                run_id,
                errcode: 500,
                description: err_msg,
            });
            metrics::counter!("acp_runs_total", "status" => "error", "agent_id" => agent_id.to_string()).increment(1);
        }
        Err(_timeout) => {
            rd.run.status = RunStatus::Timeout;
            rd.run.updated_at = Utc::now();
            rd.output = Some(RunOutput::Error {
                run_id,
                errcode: 408,
                description: format!("Run timed out after {} seconds", timeout_duration.as_secs()),
            });
            metrics::counter!("acp_runs_total", "status" => "timeout", "agent_id" => agent_id.to_string()).increment(1);
        }
    }

    // Persist final run state to SQLite
    if let Some(ref db) = state.db {
        let status_str = match rd.run.status {
            RunStatus::Success => "success",
            RunStatus::Error => "error",
            RunStatus::Timeout => "timeout",
            _ => "pending",
        };
        let output_val = rd.output.as_ref().map(|o| serde_json::to_value(o).unwrap_or_default());
        let _ = db.update_run(&run_id.to_string(), status_str, output_val.as_ref(), &rd.run.updated_at).await;
    }
}

/// Execute a streaming run — sends incremental SSE updates.
pub async fn execute_streaming_run(
    state: &AppState,
    run_id: Uuid,
    agent_id: Uuid,
    input: Option<serde_json::Value>,
    config: Option<RunConfig>,
    thread_messages: Option<Vec<Message>>,
) {
    let agent_entry = match state.agents.get(&agent_id) {
        Some(e) => e.clone(),
        None => {
            update_run_error(state, run_id, "Agent not found").await;
            return;
        }
    };

    let model = match state
        .manager
        .catalog()
        .get_model(&agent_entry.model_alias)
        .await
    {
        Ok(m) => m,
        Err(e) => {
            update_run_error(state, run_id, &format!("Model error: {e}")).await;
            return;
        }
    };

    if let Err(e) = ensure_model_ready(&model).await {
        update_run_error(state, run_id, &format!("Model load error: {e}")).await;
        return;
    }

    let messages = build_message_list(thread_messages.as_deref(), &input);
    let inf_config = extract_config(&config);

    let mut client = model.create_chat_client();
    if let Some(t) = inf_config.temperature {
        client = client.temperature(t);
    }
    if let Some(m) = inf_config.max_tokens {
        client = client.max_tokens(m);
    }
    if let Some(p) = inf_config.top_p {
        client = client.top_p(p);
    }

    let stream_result = client.complete_streaming_chat(&messages, None).await;
    match stream_result {
        Ok(mut stream) => {
            use tokio_stream::StreamExt;
            let mut accumulated = String::new();
            while let Some(result) = StreamExt::next(&mut stream).await {
                match result {
                    Ok(chunk) => {
                        for choice in &chunk.choices {
                            if let Some(ref content) = choice.delta.content {
                                accumulated.push_str(content);
                                if let Some(tx) = state.run_streams.get(&run_id) {
                                    let _ = tx.send(ValueRunResultUpdate {
                                        update_type: "values".into(),
                                        run_id,
                                        status: RunStatus::Pending,
                                        values: serde_json::json!({ "text": accumulated }),
                                        messages: None,
                                    });
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Stream chunk error: {e}");
                        break;
                    }
                }
            }

            // Final update
            let entry = match state.runs.get(&run_id) {
                Some(e) => e,
                None => return,
            };
            let mut rd = entry.lock().await;
            rd.run.status = RunStatus::Success;
            rd.run.updated_at = Utc::now();

            let response_messages = vec![Message {
                role: "assistant".into(),
                content: serde_json::Value::String(accumulated.clone()),
                id: None,
                metadata: None,
            }];

            rd.output = Some(RunOutput::Result {
                values: Some(serde_json::json!({ "text": accumulated })),
                messages: Some(response_messages),
            });

            if let Some(tx) = state.run_streams.get(&run_id) {
                let _ = tx.send(ValueRunResultUpdate {
                    update_type: "values".into(),
                    run_id,
                    status: RunStatus::Success,
                    values: serde_json::json!({ "text": accumulated }),
                    messages: None,
                });
            }

            // Persist streaming run success
            if let Some(ref db) = state.db {
                let output_val = serde_json::to_value(&rd.output).unwrap_or_default();
                let _ = db.update_run(&run_id.to_string(), "success", Some(&output_val), &rd.run.updated_at).await;
            }
        }
        Err(e) => {
            update_run_error(state, run_id, &format!("Streaming error: {e}")).await;
        }
    }
}

async fn execute_run_inner(
    state: &AppState,
    agent_id: Uuid,
    input: &Option<serde_json::Value>,
    config: &Option<RunConfig>,
    thread_messages: Option<&[Message]>,
) -> Result<(String, Vec<Message>, serde_json::Value), String> {
    let agent_entry = state
        .agents
        .get(&agent_id)
        .ok_or_else(|| format!("Agent not found: {agent_id}"))?;

    let model = state
        .manager
        .catalog()
        .get_model(&agent_entry.model_alias)
        .await
        .map_err(|e| format!("Failed to get model: {e}"))?;

    ensure_model_ready(&model).await.map_err(|e| format!("Model load error: {e}"))?;

    let messages = build_message_list(thread_messages, input);
    let inf_config = extract_config(config);

    let mut client = model.create_chat_client();
    if let Some(t) = inf_config.temperature {
        client = client.temperature(t);
    }
    if let Some(m) = inf_config.max_tokens {
        client = client.max_tokens(m);
    }
    if let Some(p) = inf_config.top_p {
        client = client.top_p(p);
    }

    let response = client
        .complete_chat(&messages, None)
        .await
        .map_err(|e| format!("Chat completion error: {e}"))?;

    let output_text = response
        .choices
        .first()
        .and_then(|c| c.message.content.as_deref())
        .unwrap_or("")
        .to_string();

    // Extract token usage
    let usage = if let Some(ref u) = response.usage {
        serde_json::json!({
            "prompt_tokens": u.prompt_tokens,
            "completion_tokens": u.completion_tokens,
            "total_tokens": u.total_tokens,
        })
    } else {
        serde_json::Value::Null
    };

    let response_messages = vec![Message {
        role: "assistant".into(),
        content: serde_json::Value::String(output_text.clone()),
        id: None,
        metadata: None,
    }];

    Ok((output_text, response_messages, usage))
}

async fn ensure_model_ready(
    model: &foundry_local_sdk::Model,
) -> Result<(), String> {
    if !model.is_cached().await.unwrap_or(false) {
        model
            .download(None::<fn(f64)>)
            .await
            .map_err(|e| format!("Download failed: {e}"))?;
    }
    if !model.is_loaded().await.unwrap_or(false) {
        model
            .load()
            .await
            .map_err(|e| format!("Load failed: {e}"))?;
    }
    Ok(())
}

async fn update_run_error(state: &AppState, run_id: Uuid, msg: &str) {
    if let Some(entry) = state.runs.get(&run_id) {
        let mut rd = entry.lock().await;
        rd.run.status = RunStatus::Error;
        rd.run.updated_at = Utc::now();
        rd.output = Some(RunOutput::Error {
            run_id,
            errcode: 500,
            description: msg.to_string(),
        });

        // Persist error state
        if let Some(ref db) = state.db {
            let output_val = serde_json::to_value(&rd.output).unwrap_or_default();
            let _ = db.update_run(&run_id.to_string(), "error", Some(&output_val), &rd.run.updated_at).await;
        }
    }
}
