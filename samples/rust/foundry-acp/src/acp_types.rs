// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! ACP data types corresponding to the Agent Connect Protocol v0.2.3 OpenAPI specification.
//! See <https://github.com/agntcy/acp-spec/blob/main/openapi.json>

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Enums ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Error,
    Success,
    Timeout,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Idle,
    Busy,
    Interrupted,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamingMode {
    Values,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnDisconnect {
    Cancel,
    Continue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultitaskStrategy {
    Reject,
    Rollback,
    Interrupt,
    Enqueue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnCompletion {
    Delete,
    Keep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IfNotExists {
    Create,
    Reject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IfExists {
    Raise,
    DoNothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelAction {
    Interrupt,
    Rollback,
}

// ── Agent types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRef {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMetadata {
    #[serde(rename = "ref")]
    pub agent_ref: AgentRef,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub agent_id: Uuid,
    pub metadata: AgentMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    #[serde(default)]
    pub threads: bool,
    #[serde(default)]
    pub interrupts: bool,
    #[serde(default)]
    pub callbacks: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming: Option<StreamingCapabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentACPSpec {
    pub capabilities: AgentCapabilities,
    pub input: serde_json::Value,
    pub output: serde_json::Value,
    pub config: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_state: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interrupts: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_streaming_update: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentACPDescriptor {
    pub metadata: AgentMetadata,
    pub specs: AgentACPSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSearchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

// ── Message ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

// ── Thread types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub thread_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub status: ThreadStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadCreate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default = "default_if_exists")]
    pub if_exists: IfExists,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<ThreadCheckpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadCheckpoint {
    pub checkpoint_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadState {
    pub checkpoint: ThreadCheckpoint,
    pub values: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSearchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ThreadStatus>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

// ── Run types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursion_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configurable: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCreate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<RunConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_mode: Option<serde_json::Value>,
    #[serde(default = "default_on_disconnect")]
    pub on_disconnect: OnDisconnect,
    #[serde(default = "default_multitask")]
    pub multitask_strategy: MultitaskStrategy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCreateStateless {
    #[serde(flatten)]
    pub base: RunCreate,
    #[serde(default = "default_on_completion")]
    pub on_completion: OnCompletion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCreateStateful {
    #[serde(flatten)]
    pub base: RunCreate,
    #[serde(default)]
    pub stream_subgraphs: bool,
    #[serde(default = "default_if_not_exists")]
    pub if_not_exists: IfNotExists,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub run_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<Uuid>,
    pub agent_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub status: RunStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStateless {
    #[serde(flatten)]
    pub run: Run,
    pub creation: RunCreateStateless,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStateful {
    #[serde(flatten)]
    pub run: Run,
    pub creation: RunCreateStateful,
}

// ── Run output types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunOutput {
    Result {
        #[serde(skip_serializing_if = "Option::is_none")]
        values: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        messages: Option<Vec<Message>>,
    },
    Interrupt {
        interrupt: serde_json::Value,
    },
    Error {
        run_id: Uuid,
        errcode: i64,
        description: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunWaitResponseStateless {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<RunStateless>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<RunOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunWaitResponseStateful {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<RunStateful>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<RunOutput>,
}

// ── SSE streaming types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueRunResultUpdate {
    #[serde(rename = "type")]
    pub update_type: String,
    pub run_id: Uuid,
    pub status: RunStatus,
    pub values: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<Message>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunOutputStream {
    pub id: String,
    pub event: String,
    pub data: serde_json::Value,
}

// ── Search requests ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSearchRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<RunStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumePayload {
    #[serde(flatten)]
    pub data: serde_json::Value,
}

// ── Defaults ─────────────────────────────────────────────────────────────────

fn default_limit() -> i64 {
    10
}
fn default_on_disconnect() -> OnDisconnect {
    OnDisconnect::Cancel
}
fn default_multitask() -> MultitaskStrategy {
    MultitaskStrategy::Reject
}
fn default_on_completion() -> OnCompletion {
    OnCompletion::Delete
}
fn default_if_not_exists() -> IfNotExists {
    IfNotExists::Reject
}
fn default_if_exists() -> IfExists {
    IfExists::Raise
}
