// Copyright (c) Microsoft Corporation. All rights reserved.
// Licensed under the MIT License.

//! Integration tests for the Foundry ACP server.
//!
//! These tests validate compliance with the Agent Connect Protocol v0.2.3
//! by starting a real ACP server in-process backed by Foundry Local with
//! the `qwen2.5-0.5b` model.
//!
//! The tests are marked `#[ignore]` because they require:
//!   - Foundry Local runtime installed on the machine
//!   - Network access to download the model on first run
//!   - Sufficient disk space for the qwen2.5-0.5b model (~500 MB)
//!
//! Run with:
//!   cargo test -p foundry-acp --test acp_integration -- --ignored --test-threads=1

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::Client;
use serde_json::{json, Value};
use tokio::net::TcpListener;

use foundry_local_sdk::{FoundryLocalConfig, FoundryLocalManager};

const TEST_MODEL: &str = "qwen2.5-0.5b";

/// The base URL of the in-process server, set once during init.
static BASE_URL: OnceLock<String> = OnceLock::new();

/// Start the ACP server once on a random available port.
async fn ensure_server() -> &'static str {
    if let Some(url) = BASE_URL.get() {
        return url;
    }

    // Find a free port
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let base = format!("http://127.0.0.1:{port}");
    let _ = BASE_URL.set(base);

    // Initialise SDK + state
    let manager =
        FoundryLocalManager::create(FoundryLocalConfig::new("foundry_acp_test")).unwrap();
    let state = foundry_acp::init_state(manager, Some(TEST_MODEL))
        .await
        .expect("Failed to initialise ACP state — is Foundry Local installed?");
    let app = foundry_acp::build_router(state);

    // Start server in background
    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Wait for ready
    let client = Client::new();
    let url = BASE_URL.get().unwrap().as_str();
    for _ in 0..40 {
        if client
            .post(&format!("{url}/agents/search"))
            .json(&json!({}))
            .send()
            .await
            .is_ok()
        {
            return url;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("ACP server did not become ready within 10 seconds");
}

fn url(path: &str) -> String {
    format!("{}{path}", BASE_URL.get().unwrap())
}

// ── Agent Tests ──────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_search_agents_returns_array() {
    ensure_server().await;
    let client = Client::new();
    let resp = client
        .post(&url("/agents/search"))
        .json(&json!({}))
        .send()
        .await
        .expect("Failed to reach server");

    assert_eq!(resp.status(), 200);
    let body: Vec<Value> = resp.json().await.unwrap();
    assert!(!body.is_empty(), "Should have at least one agent");

    let agent = &body[0];
    assert!(agent.get("agent_id").is_some(), "agent must have agent_id");
    assert!(agent.get("metadata").is_some(), "agent must have metadata");

    let metadata = &agent["metadata"];
    assert!(metadata.get("ref").is_some(), "metadata must have ref");
    assert!(
        metadata.get("description").is_some(),
        "metadata must have description"
    );

    let agent_ref = &metadata["ref"];
    assert!(agent_ref.get("name").is_some(), "ref must have name");
    assert!(agent_ref.get("version").is_some(), "ref must have version");
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_search_agents_with_name_filter() {
    ensure_server().await;
    let client = Client::new();
    let resp = client
        .post(&url("/agents/search"))
        .json(&json!({ "name": TEST_MODEL }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Vec<Value> = resp.json().await.unwrap();
    assert!(
        !body.is_empty(),
        "Should find agent matching '{TEST_MODEL}'"
    );
    assert!(
        body[0]["metadata"]["ref"]["name"]
            .as_str()
            .unwrap()
            .contains(TEST_MODEL),
        "Filtered agent name should contain '{TEST_MODEL}'"
    );
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_search_agents_pagination() {
    ensure_server().await;
    let client = Client::new();
    let resp = client
        .post(&url("/agents/search"))
        .json(&json!({ "limit": 1, "offset": 0 }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: Vec<Value> = resp.json().await.unwrap();
    assert!(body.len() <= 1, "Limit should cap results to 1");
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_get_agent_by_id() {
    ensure_server().await;
    let client = Client::new();

    let search_resp = client
        .post(&url("/agents/search"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let agents: Vec<Value> = search_resp.json().await.unwrap();
    let agent_id = agents[0]["agent_id"].as_str().unwrap();

    let resp = client
        .get(&url(&format!("/agents/{agent_id}")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let agent: Value = resp.json().await.unwrap();
    assert_eq!(agent["agent_id"].as_str().unwrap(), agent_id);
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_get_agent_not_found() {
    ensure_server().await;
    let client = Client::new();
    let fake_id = "00000000-0000-0000-0000-000000000000";
    let resp = client
        .get(&url(&format!("/agents/{fake_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_get_agent_descriptor() {
    ensure_server().await;
    let client = Client::new();

    let search_resp = client
        .post(&url("/agents/search"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let agents: Vec<Value> = search_resp.json().await.unwrap();
    let agent_id = agents[0]["agent_id"].as_str().unwrap();

    let resp = client
        .get(&url(&format!("/agents/{agent_id}/descriptor")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let descriptor: Value = resp.json().await.unwrap();

    // Validate ACP descriptor structure
    assert!(
        descriptor.get("metadata").is_some(),
        "descriptor must have metadata"
    );
    assert!(
        descriptor.get("specs").is_some(),
        "descriptor must have specs"
    );

    let specs = &descriptor["specs"];
    assert!(
        specs.get("capabilities").is_some(),
        "specs must have capabilities"
    );
    assert!(specs.get("input").is_some(), "specs must have input schema");
    assert!(
        specs.get("output").is_some(),
        "specs must have output schema"
    );
    assert!(
        specs.get("config").is_some(),
        "specs must have config schema"
    );

    // Validate capabilities
    let caps = &specs["capabilities"];
    assert_eq!(caps["threads"].as_bool(), Some(true));
    assert_eq!(caps["interrupts"].as_bool(), Some(false));
    assert!(caps.get("streaming").is_some());
    assert_eq!(caps["streaming"]["values"].as_bool(), Some(true));
}

// ── Thread Tests ─────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_thread_lifecycle() {
    ensure_server().await;
    let client = Client::new();

    // Create thread
    let resp = client
        .post(&url("/threads"))
        .json(&json!({ "metadata": { "test": true } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let thread: Value = resp.json().await.unwrap();
    let thread_id = thread["thread_id"].as_str().unwrap().to_string();
    assert_eq!(thread["status"].as_str().unwrap(), "idle");
    assert!(thread.get("created_at").is_some());
    assert!(thread.get("updated_at").is_some());

    // Get thread
    let resp = client
        .get(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Patch thread
    let resp = client
        .patch(&url(&format!("/threads/{thread_id}")))
        .json(&json!({ "metadata": { "updated": true } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let patched: Value = resp.json().await.unwrap();
    assert_eq!(patched["metadata"]["updated"].as_bool(), Some(true));
    assert_eq!(patched["metadata"]["test"].as_bool(), Some(true));

    // Copy thread
    let resp = client
        .post(&url(&format!("/threads/{thread_id}/copy")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let copy: Value = resp.json().await.unwrap();
    assert_ne!(copy["thread_id"].as_str(), Some(thread_id.as_str()));

    // Delete copy
    let copy_id = copy["thread_id"].as_str().unwrap();
    let resp = client
        .delete(&url(&format!("/threads/{copy_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Delete original
    let resp = client
        .delete(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Verify deleted
    let resp = client
        .get(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_thread_duplicate_handling() {
    ensure_server().await;
    let client = Client::new();
    let thread_id = uuid::Uuid::new_v4().to_string();

    // Create
    let resp = client
        .post(&url("/threads"))
        .json(&json!({ "thread_id": thread_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Duplicate with raise (default)
    let resp = client
        .post(&url("/threads"))
        .json(&json!({ "thread_id": thread_id }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 409);

    // Duplicate with do_nothing
    let resp = client
        .post(&url("/threads"))
        .json(&json!({ "thread_id": thread_id, "if_exists": "do_nothing" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Cleanup
    client
        .delete(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_search_threads() {
    ensure_server().await;
    let client = Client::new();

    let resp = client
        .post(&url("/threads"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let thread: Value = resp.json().await.unwrap();
    let thread_id = thread["thread_id"].as_str().unwrap().to_string();

    let resp = client
        .post(&url("/threads/search"))
        .json(&json!({ "status": "idle" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let results: Vec<Value> = resp.json().await.unwrap();
    assert!(!results.is_empty());

    // Cleanup
    client
        .delete(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
}

// ── Stateless Run Tests ──────────────────────────────────────────────────────

/// Helper: find the first agent_id from the catalog.
async fn get_first_agent_id(client: &Client) -> String {
    let resp = client
        .post(&url("/agents/search"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let agents: Vec<Value> = resp.json().await.unwrap();
    assert!(!agents.is_empty(), "No agents available — is the model loaded?");
    agents[0]["agent_id"].as_str().unwrap().to_string()
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_create_stateless_run_wait() {
    ensure_server().await;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();

    let agent_id = get_first_agent_id(&client).await;

    let resp = client
        .post(&url("/runs/wait"))
        .json(&json!({
            "agent_id": agent_id,
            "input": {
                "messages": [
                    { "role": "user", "content": "Say hello in exactly 3 words." }
                ]
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.unwrap();

    // Validate run structure per ACP spec
    let run = &result["run"];
    assert!(run.get("run_id").is_some(), "run must have run_id");
    assert_eq!(run["agent_id"].as_str().unwrap(), agent_id);
    assert_eq!(run["status"].as_str().unwrap(), "success");
    assert!(run.get("created_at").is_some());
    assert!(run.get("updated_at").is_some());

    // Validate output
    let output = &result["output"];
    assert!(
        output.get("values").is_some() || output.get("messages").is_some(),
        "output must have values or messages"
    );
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_create_background_run_and_poll() {
    ensure_server().await;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();

    let agent_id = get_first_agent_id(&client).await;

    // Create background run (POST /runs)
    let resp = client
        .post(&url("/runs"))
        .json(&json!({
            "agent_id": agent_id,
            "input": {
                "messages": [
                    { "role": "user", "content": "Say hi" }
                ]
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let run: Value = resp.json().await.unwrap();
    let run_id = run["run_id"].as_str().unwrap();
    assert_eq!(run["status"].as_str().unwrap(), "pending");

    // Wait for completion (GET /runs/{id}/wait)
    let resp = client
        .get(&url(&format!("/runs/{run_id}/wait")))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.unwrap();
    assert_eq!(result["run"]["status"].as_str().unwrap(), "success");

    // GET /runs/{id}
    let resp = client
        .get(&url(&format!("/runs/{run_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // DELETE /runs/{id}
    let resp = client
        .delete(&url(&format!("/runs/{run_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Verify deleted
    let resp = client
        .get(&url(&format!("/runs/{run_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_run_not_found() {
    ensure_server().await;
    let client = Client::new();
    let fake_id = "00000000-0000-0000-0000-000000000000";
    let resp = client
        .get(&url(&format!("/runs/{fake_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_search_stateless_runs() {
    ensure_server().await;
    let client = Client::new();
    let resp = client
        .post(&url("/runs/search"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let _results: Vec<Value> = resp.json().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_cancel_run() {
    ensure_server().await;
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    let agent_id = get_first_agent_id(&client).await;

    // Create run
    let resp = client
        .post(&url("/runs"))
        .json(&json!({
            "agent_id": agent_id,
            "input": {
                "messages": [
                    { "role": "user", "content": "Write a very long essay about quantum physics." }
                ]
            }
        }))
        .send()
        .await
        .unwrap();
    let run: Value = resp.json().await.unwrap();
    let run_id = run["run_id"].as_str().unwrap();

    // Cancel it
    let resp = client
        .post(&url(&format!("/runs/{run_id}/cancel")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_resume_not_supported() {
    ensure_server().await;
    let client = Client::new();
    let fake_id = uuid::Uuid::new_v4();
    let resp = client
        .post(&url(&format!("/runs/{fake_id}")))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 422);
}

// ── Thread Run Tests ─────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_thread_run_wait() {
    ensure_server().await;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();

    // Create thread
    let resp = client
        .post(&url("/threads"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let thread: Value = resp.json().await.unwrap();
    let thread_id = thread["thread_id"].as_str().unwrap();

    let agent_id = get_first_agent_id(&client).await;

    // Create and wait for thread run
    let resp = client
        .post(&url(&format!("/threads/{thread_id}/runs/wait")))
        .json(&json!({
            "agent_id": agent_id,
            "input": {
                "messages": [
                    { "role": "user", "content": "What is 2+2?" }
                ]
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.unwrap();
    assert_eq!(result["run"]["status"].as_str().unwrap(), "success");
    assert!(result.get("output").is_some());

    // Thread should be back to idle
    let resp = client
        .get(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
    let thread: Value = resp.json().await.unwrap();
    assert_eq!(thread["status"].as_str().unwrap(), "idle");

    // Thread should have messages
    if let Some(msgs) = thread["messages"].as_array() {
        assert!(
            msgs.len() >= 2,
            "Thread should have at least user + assistant messages"
        );
    }

    // Check thread history
    let resp = client
        .get(&url(&format!("/threads/{thread_id}/history")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let history: Vec<Value> = resp.json().await.unwrap();
    assert!(!history.is_empty(), "Thread should have history");

    // List thread runs
    let resp = client
        .get(&url(&format!("/threads/{thread_id}/runs")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Cleanup
    client
        .delete(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_multi_turn_thread_conversation() {
    ensure_server().await;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();

    // Create thread
    let resp = client
        .post(&url("/threads"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    let thread: Value = resp.json().await.unwrap();
    let thread_id = thread["thread_id"].as_str().unwrap();

    let agent_id = get_first_agent_id(&client).await;

    // Turn 1
    let resp = client
        .post(&url(&format!("/threads/{thread_id}/runs/wait")))
        .json(&json!({
            "agent_id": agent_id,
            "input": {
                "messages": [
                    { "role": "user", "content": "My name is Alice." }
                ]
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Turn 2 — should have context from Turn 1
    let resp = client
        .post(&url(&format!("/threads/{thread_id}/runs/wait")))
        .json(&json!({
            "agent_id": agent_id,
            "input": {
                "messages": [
                    { "role": "user", "content": "What is my name?" }
                ]
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.unwrap();
    assert_eq!(result["run"]["status"].as_str().unwrap(), "success");

    // Thread should now have 4+ messages (2 user + 2 assistant)
    let resp = client
        .get(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
    let thread: Value = resp.json().await.unwrap();
    if let Some(msgs) = thread["messages"].as_array() {
        assert!(
            msgs.len() >= 4,
            "Multi-turn thread should have at least 4 messages, got {}",
            msgs.len()
        );
    }

    // Cleanup
    client
        .delete(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_thread_run_if_not_exists_create() {
    ensure_server().await;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();

    let thread_id = uuid::Uuid::new_v4().to_string();
    let agent_id = get_first_agent_id(&client).await;

    // Run on non-existent thread with if_not_exists: create
    let resp = client
        .post(&url(&format!("/threads/{thread_id}/runs/wait")))
        .json(&json!({
            "agent_id": agent_id,
            "if_not_exists": "create",
            "input": {
                "messages": [
                    { "role": "user", "content": "Hello" }
                ]
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Thread should now exist
    let resp = client
        .get(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Cleanup
    client
        .delete(&url(&format!("/threads/{thread_id}")))
        .send()
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_thread_run_if_not_exists_reject() {
    ensure_server().await;
    let client = Client::new();
    let thread_id = uuid::Uuid::new_v4().to_string();
    let agent_id = get_first_agent_id(&client).await;

    // Run on non-existent thread with if_not_exists: reject
    let resp = client
        .post(&url(&format!("/threads/{thread_id}/runs/wait")))
        .json(&json!({
            "agent_id": agent_id,
            "if_not_exists": "reject",
            "input": {
                "messages": [
                    { "role": "user", "content": "Hello" }
                ]
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

// ── Streaming Tests ──────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_stateless_run_stream() {
    ensure_server().await;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();

    let agent_id = get_first_agent_id(&client).await;

    let resp = client
        .post(&url("/runs/stream"))
        .json(&json!({
            "agent_id": agent_id,
            "input": {
                "messages": [
                    { "role": "user", "content": "Count from 1 to 5." }
                ]
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap_or("").contains("text/event-stream"))
            .unwrap_or(false),
        "Response should be SSE"
    );

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("agent_event") || body.contains("values"),
        "Stream should contain ACP events, got: {body}"
    );
}

// ── Config Tests ─────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_run_with_config() {
    ensure_server().await;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();

    let agent_id = get_first_agent_id(&client).await;

    let resp = client
        .post(&url("/runs/wait"))
        .json(&json!({
            "agent_id": agent_id,
            "input": {
                "messages": [
                    { "role": "user", "content": "Say exactly: test" }
                ]
            },
            "config": {
                "configurable": {
                    "temperature": 0.0,
                    "max_tokens": 10
                }
            }
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.unwrap();
    assert_eq!(result["run"]["status"].as_str().unwrap(), "success");
}

// ── Input Format Tests ───────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Foundry Local runtime"]
async fn test_run_with_string_input() {
    ensure_server().await;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();

    let agent_id = get_first_agent_id(&client).await;

    // String input (should be treated as a single user message)
    let resp = client
        .post(&url("/runs/wait"))
        .json(&json!({
            "agent_id": agent_id,
            "input": "Hello, what is 1+1?"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let result: Value = resp.json().await.unwrap();
    assert_eq!(result["run"]["status"].as_str().unwrap(), "success");
}
