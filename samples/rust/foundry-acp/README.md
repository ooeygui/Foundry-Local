# Foundry ACP — Agent Communication Protocol Server for Foundry Local

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

**Foundry ACP** exposes [Foundry Local](https://github.com/microsoft/Foundry-Local) AI models as standards-compliant [Agent Connect Protocol (ACP)](https://agntcy.org/) agents. Any ACP client can discover, invoke, and converse with local models through a REST API — no cloud required.

## Overview

```
┌─────────────────┐         HTTP/REST          ┌──────────────────┐
│   ACP Client    │ ◄─────────────────────────► │   foundry-acp    │
│ (any language)  │   Agent Connect Protocol    │   (this server)  │
└─────────────────┘         v0.2.3              └────────┬─────────┘
                                                         │
                                                         │ Foundry Local SDK (FFI)
                                                         ▼
                                                ┌──────────────────┐
                                                │  Foundry Local   │
                                                │  Runtime + Model │
                                                └──────────────────┘
```

Each Foundry Local model is registered as an ACP **Agent** with:
- A deterministic UUID (derived from the model alias)
- A full descriptor declaring input/output/config schemas and capabilities
- Support for stateless runs, stateful threaded conversations, and SSE streaming

## Features

| Feature | Status |
|---------|--------|
| Agent discovery & descriptors | ✅ |
| Stateless runs (create, wait, stream) | ✅ |
| Threaded conversations (multi-turn) | ✅ |
| SSE streaming with replacement semantics | ✅ |
| Run lifecycle (poll, cancel, delete) | ✅ |
| Thread CRUD (create, get, patch, copy, delete) | ✅ |
| Thread history & search | ✅ |
| Configurable inference (temperature, max_tokens, top_p) | ✅ |
| Run timeout enforcement | ✅ |
| Concurrent run rate limiting (semaphore) | ✅ |
| Token usage tracking in run output | ✅ |
| Lazy model loading (download + load on first request) | ✅ |
| Health endpoint (`GET /health`) | ✅ |
| Prometheus metrics (`GET /metrics`) | ✅ |
| OpenAPI spec endpoint (`GET /openapi.json`) | ✅ |
| Request tracing (structured per-request logging) | ✅ |
| SQLite persistent storage (threads + runs survive restarts) | ✅ |
| TLS support (`--cert`/`--key`) | ✅ (feature-gated) |
| Windows service install/uninstall | ✅ (feature-gated) |
| CORS (permissive, for local dev) | ✅ |
| GitHub Actions CI workflow | ✅ |
| Resume/interrupt | 🚫 Returns 422 (per spec) |
| Checkpoints | 🚫 Not implemented |

## Prerequisites

- **Foundry Local** runtime installed ([installation guide](https://github.com/microsoft/Foundry-Local#installation))
- **Rust 1.75+** (for building from source)
- A supported model (e.g., `qwen2.5-0.5b`) — will be auto-downloaded on first use

## Quick Start

```bash
# Build
cd samples/rust
cargo build -p foundry-acp --release

# Run (loads all catalog models as agents)
cargo run -p foundry-acp --release -- --port 8080

# Run with a specific model
cargo run -p foundry-acp --release -- --model qwen2.5-0.5b --port 8080
```

The server starts at `http://127.0.0.1:8080` and logs registered agents:

```
INFO Foundry ACP server listening on http://127.0.0.1:8080
INFO ACP spec version: 0.2.3
INFO Registered 3 ACP agents
INFO   Agent: qwen2.5-0.5b (alias=qwen2.5-0.5b, id=a1b2c3d4-...)
```

## CLI Reference

```
foundry-acp [OPTIONS]

Options:
  --host <HOST>              Host address to bind to [default: 127.0.0.1]
  --port <PORT>              Port to listen on [default: 8088]
  --model <ALIAS>            Load only this model (loads all if omitted)
  --timeout <SECONDS>        Max run timeout in seconds; 0 = unlimited [default: 300]
  --max-concurrent-runs <N>  Max concurrent runs; 0 = unlimited [default: 64]
  --db <PATH>                SQLite database path for persistent storage (omit for in-memory)
  --cert <FILE>              TLS certificate (PEM); requires --features tls
  --key <FILE>               TLS private key (PEM); requires --features tls
  --install-service          Install as a Windows service (requires --features windows-service)
  --uninstall-service        Uninstall the Windows service
  -h, --help                 Print help
  -V, --version              Print version
```

### Environment Variables

| Variable | Effect |
|----------|--------|
| `RUST_LOG` | Controls log verbosity (e.g., `RUST_LOG=debug`, `RUST_LOG=foundry_acp=trace`) |

### Feature Flags

| Flag | Effect |
|------|--------|
| `tls` | Enables `--cert`/`--key` flags for HTTPS via rustls |
| `windows-service` | Enables `--install-service`/`--uninstall-service` flags |

Build with features:
```bash
cargo build -p foundry-acp --release --features tls,windows-service
```

## API Endpoints

All endpoints follow the [ACP v0.2.3 specification](https://github.com/agntcy/acp-spec).

### Agents

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/agents/search` | Search/list agents (with optional name filter, pagination) |
| `GET` | `/agents/{agent_id}` | Get agent by ID |
| `GET` | `/agents/{agent_id}/descriptor` | Get full agent descriptor (schemas, capabilities) |

### Stateless Runs

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/runs` | Create a background run (returns immediately with `pending` status) |
| `POST` | `/runs/wait` | Create and wait for completion (synchronous) |
| `POST` | `/runs/stream` | Create and stream results via SSE |
| `POST` | `/runs/search` | Search runs by agent/status/metadata |
| `GET` | `/runs/{run_id}` | Get run status and result |
| `GET` | `/runs/{run_id}/wait` | Wait for a pending run to complete |
| `GET` | `/runs/{run_id}/stream` | Attach to a run's SSE stream |
| `POST` | `/runs/{run_id}/cancel` | Cancel a pending/running run |
| `DELETE` | `/runs/{run_id}` | Delete a completed run |
| `POST` | `/runs/{run_id}` | Resume (returns 422 — not supported) |

### Threads

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/threads` | Create a new thread |
| `POST` | `/threads/search` | Search threads by status/metadata |
| `GET` | `/threads/{thread_id}` | Get thread state (including messages) |
| `PATCH` | `/threads/{thread_id}` | Update thread metadata |
| `DELETE` | `/threads/{thread_id}` | Delete a thread |
| `POST` | `/threads/{thread_id}/copy` | Deep-copy a thread |
| `GET` | `/threads/{thread_id}/history` | Get thread state history (checkpoints) |

### Thread Runs

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/threads/{thread_id}/runs` | Create a thread run (background) |
| `POST` | `/threads/{thread_id}/runs/wait` | Create and wait for thread run |
| `POST` | `/threads/{thread_id}/runs/stream` | Create and stream thread run via SSE |
| `GET` | `/threads/{thread_id}/runs` | List runs for a thread |
| `GET` | `/threads/{thread_id}/runs/{run_id}` | Get a specific thread run |
| `GET` | `/threads/{thread_id}/runs/{run_id}/wait` | Wait for a thread run |
| `GET` | `/threads/{thread_id}/runs/{run_id}/stream` | Attach to thread run SSE stream |
| `POST` | `/threads/{thread_id}/runs/{run_id}/cancel` | Cancel a thread run |
| `DELETE` | `/threads/{thread_id}/runs/{run_id}` | Delete a thread run |

### Operational

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Health probe (status, uptime, model readiness) |
| `GET` | `/metrics` | Prometheus metrics (request counts, latencies) |
| `GET` | `/openapi.json` | Full ACP OpenAPI 3.1 specification |

## Usage Examples

### Discover available agents

```bash
curl -X POST http://localhost:8080/agents/search \
  -H "Content-Type: application/json" \
  -d '{}'
```

### Simple completion (synchronous)

```bash
curl -X POST http://localhost:8080/runs/wait \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "YOUR_AGENT_UUID",
    "input": {
      "messages": [
        {"role": "user", "content": "Explain quantum computing in 3 sentences."}
      ]
    }
  }'
```

### Completion with config

```bash
curl -X POST http://localhost:8080/runs/wait \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "YOUR_AGENT_UUID",
    "input": {
      "messages": [{"role": "user", "content": "Hello"}]
    },
    "config": {
      "configurable": {
        "temperature": 0.7,
        "max_tokens": 256,
        "top_p": 0.9
      }
    }
  }'
```

### Streaming (SSE)

```bash
curl -N -X POST http://localhost:8080/runs/stream \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "YOUR_AGENT_UUID",
    "input": {
      "messages": [{"role": "user", "content": "Count from 1 to 10."}]
    }
  }'
```

The response is a `text/event-stream` with events:

```
event: agent_event
data: {"values":{"messages":[{"role":"assistant","content":"1"}]}}

event: agent_event
data: {"values":{"messages":[{"role":"assistant","content":"1, 2"}]}}

...

event: done
data: {"run":{"run_id":"...","status":"success",...},"output":{...}}
```

> **Note**: Per ACP spec, each `agent_event` sends the **full accumulated output** (replacement semantics), not deltas.

### Multi-turn conversation (threads)

```bash
# 1. Create a thread
THREAD_ID=$(curl -s -X POST http://localhost:8080/threads \
  -H "Content-Type: application/json" \
  -d '{}' | jq -r '.thread_id')

# 2. First turn
curl -X POST http://localhost:8080/threads/$THREAD_ID/runs/wait \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "YOUR_AGENT_UUID",
    "input": {"messages": [{"role": "user", "content": "My name is Alice."}]}
  }'

# 3. Second turn (model has context from turn 1)
curl -X POST http://localhost:8080/threads/$THREAD_ID/runs/wait \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "YOUR_AGENT_UUID",
    "input": {"messages": [{"role": "user", "content": "What is my name?"}]}
  }'
```

### String input shorthand

Instead of the `messages` array, you can pass a plain string as input:

```bash
curl -X POST http://localhost:8080/runs/wait \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "YOUR_AGENT_UUID",
    "input": "What is the capital of France?"
  }'
```

## Windows Service

Build with the `windows-service` feature:

```bash
cargo build -p foundry-acp --release --features windows-service
```

Install and manage:

```powershell
# Install (requires Administrator)
.\foundry-acp.exe --install-service

# Uninstall
.\foundry-acp.exe --uninstall-service

# The service runs as "Foundry ACP" in services.msc
```

When running as a service, it binds to `127.0.0.1:8088` by default. Configure via the service registry or a future config file.

## Architecture

```
src/
├── lib.rs            # Public API (build_router, init_state) for embedding/testing
├── main.rs           # CLI entry point (clap)
├── acp_types.rs      # All ACP data types from the OpenAPI spec
├── state.rs          # AppState: agent registry, thread/run stores, SDK reference
├── agents.rs         # Agent handlers (search, get, descriptor)
├── runs.rs           # Stateless run handlers + SSE streaming
├── threads.rs        # Thread CRUD + thread run handlers
├── inference.rs      # Bridge: ACP input → Foundry chat completions (with timeout/rate limit)
├── error.rs          # AcpError with HTTP status mapping
├── health.rs         # GET /health readiness probe
├── metrics.rs        # Prometheus metrics + per-request tracking middleware
├── openapi.rs        # GET /openapi.json serving embedded spec
├── openapi.json      # Embedded ACP OpenAPI specification
├── persistence.rs    # SQLite storage layer (threads + runs)
└── service.rs        # Windows service support (feature-gated)
tests/
└── acp_integration.rs  # 20+ self-contained integration tests
.github/
└── workflows/ci.yml    # GitHub Actions: check, build, integration test
```

### Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| Agent ID = UUID5(namespace, alias) | Deterministic across restarts; same model always gets same ID |
| DashMap + optional SQLite | In-memory for speed; SQLite opt-in for persistence across restarts |
| Per-thread `Arc<Mutex>` | Enforces idle→busy state transitions atomically (only one run per thread) |
| Broadcast channels for SSE | Multiple subscribers can attach to a run's stream at any time |
| Streaming sends full replacement values | Per ACP spec — not deltas like OpenAI's API |
| Tokio semaphore for rate limiting | Backpressure at 64 concurrent runs (configurable) |
| `tokio::time::timeout` per run | Automatic timeout → RunStatus::Timeout after N seconds |
| Lazy model loading in `ensure_model_ready` | Models download+load on first inference if not already ready |
| Token usage in run output | `usage.prompt_tokens`, `completion_tokens`, `total_tokens` |
| TLS + Windows service behind feature flags | Avoids pulling heavy deps for builds that don't need them |
| Prometheus metrics via `metrics` crate | Standard `/metrics` endpoint with request counts and latencies |

### How ACP Maps to Foundry

| ACP Concept | Foundry Equivalent |
|-------------|-------------------|
| Agent | Model (from catalog) |
| Run | Chat completion request |
| Thread | Conversation (message history) |
| Input | Chat messages array |
| Output | Assistant response |
| Config | Temperature, max_tokens, top_p |

## Testing

### Unit tests

```bash
cargo test -p foundry-acp
```

### Integration tests

Integration tests start a real ACP server in-process, download the `qwen2.5-0.5b` model, and validate the full ACP protocol flow:

```bash
# Run all integration tests (requires Foundry Local runtime)
cargo test -p foundry-acp --test acp_integration -- --ignored --test-threads=1
```

> **First run** may take several minutes as it downloads the model (~500 MB). Subsequent runs use the cached model.

The integration test suite covers:

- **Agent discovery**: search, filter by name, pagination, get by ID, descriptors
- **Thread lifecycle**: create, get, patch, copy, delete, duplicate handling, search
- **Stateless runs**: create/wait, background poll, cancel, delete, search
- **Thread runs**: create/wait with conversation context, multi-turn continuity
- **Streaming**: SSE content-type validation, event format
- **Config**: temperature/max_tokens passthrough
- **Input formats**: messages array and string shorthand
- **Error cases**: 404 on missing resources, 409 on duplicates, 422 on unsupported operations

## Troubleshooting

| Problem | Solution |
|---------|----------|
| "Failed to initialise SDK" | Ensure Foundry Local runtime is installed |
| No agents registered | Check that models appear in `foundry model list` |
| Model download timeout | First run needs internet; subsequent runs use cache |
| Port already in use | Change with `--port <PORT>` |
| Permission denied (service install) | Run as Administrator |

## License

MIT — see [LICENSE](../../../LICENSE) for details.
