# Copilot Proxy — OpenAI-Compatible Server for Foundry Local

An ASP.NET Core proxy server that wraps Foundry Local and provides full OpenAI API compatibility for GitHub Copilot CLI, Copilot SDK, and other OpenAI-compatible tools.

## Features

- **SSE Streaming** — Proper Server-Sent Events streaming for `/v1/chat/completions` with guaranteed `finish_reason` and `[DONE]` termination
- **Tool / Function Calling** — Correctly forwards `tool_calls` deltas in streaming, ensures `finish_reason: "tool_calls"` when tools are invoked
- **Usage Stats** — Injects `usage` object (prompt_tokens, completion_tokens, total_tokens) when `stream_options.include_usage` is requested
- **Request Sanitization** — Strips `stream_options`, `parallel_tool_calls`, `service_tier` and other fields that Foundry doesn't support, preventing request rejection
- **OpenAI Error Format** — All errors are returned in the standard `{"error": {"message": "...", "type": "...", ...}}` format
- **WebSocket Realtime API** — WebSocket endpoint at `/v1/realtime` implementing the OpenAI Realtime API protocol (text modality)
- **Path Compatibility** — Supports both `/v1/chat/completions` (OpenAI standard) and `/v1/chat_completions` (Foundry native)
- **Auth Passthrough** — Accepts `Authorization: Bearer` headers without rejection (required by Copilot SDK)
- **CORS** — Wide-open CORS headers for local development tools

## Why This Exists

Foundry Local's built-in web service has compatibility gaps that cause issues with GitHub Copilot:

1. Streaming responses sometimes omit `finish_reason`, causing Copilot SDK errors
2. No WebSocket support for the Realtime API
3. Streams may not always terminate with `data: [DONE]`
4. Unknown request fields (like `stream_options`) may cause rejections
5. Error responses aren't in the OpenAI standard format
6. Tool calling `finish_reason` may not be set correctly

This proxy sits between Copilot and Foundry Local, fixing these issues transparently.

## Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/chat/completions` | POST | Chat completions with SSE streaming & tool calling |
| `/v1/chat_completions` | POST | Alias for Foundry-native path |
| `/v1/models` | GET | List available models |
| `/v1/models/{id}` | GET | Get model details |
| `/v1/responses` | POST | OpenAI Responses API |
| `/v1/realtime` | WebSocket | Realtime API (text modality) |
| `/health` | GET | Health check |

## Quick Start

```bash
# From the repo root, build the SDK first (one-time)
dotnet pack sdk/cs/src -o local-packages /p:Version=0.9.0-dev

# Build and run the proxy
cd samples/cs/copilot-proxy
dotnet build -r win-x64
dotnet run -r win-x64 -- 5001
```

## Configuration

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `FOUNDRY_URL` | Foundry Local backend URL | `http://127.0.0.1:5273` |
| `FOUNDRY_MODEL` | Model alias to load | `phi-4-mini` |
| `SKIP_EP_DOWNLOAD` | Set to `1` to skip GPU EP downloads | (not set) |
| `COPILOT_PROVIDER_BASE_URL` | Alternative to `FOUNDRY_URL` (Copilot convention) | — |
| `COPILOT_MODEL` | Alternative to `FOUNDRY_MODEL` (Copilot convention) | — |

### GitHub Copilot CLI Setup

After starting the proxy, configure Copilot CLI to use it:

```bash
# Point Copilot CLI at the proxy
export COPILOT_PROVIDER_BASE_URL="http://localhost:5001/v1"
export COPILOT_MODEL="phi-4-mini"
export COPILOT_PROVIDER_TYPE="openai"
export COPILOT_PROVIDER_API_KEY="local"

# Optional: fully offline mode (no GitHub telemetry)
export COPILOT_OFFLINE="true"

# Optional: tune context window for your model
export COPILOT_PROVIDER_MAX_PROMPT_TOKENS="16384"
export COPILOT_PROVIDER_MAX_OUTPUT_TOKENS="4096"
```

### Copilot SDK (JavaScript)

```typescript
import { CopilotClient, approveAll } from "@github/copilot-sdk";

const client = new CopilotClient();
const session = await client.createSession({
    onPermissionRequest: approveAll,
    model: "phi-4-mini",
    provider: {
        type: "openai",
        baseUrl: "http://localhost:5001/v1",
        apiKey: "local",
        wireApi: "completions",
    },
    streaming: true,
});

await session.sendAndWait({ prompt: "Hello!" });
```

### Test with curl

```bash
# Non-streaming
curl http://localhost:5001/v1/chat/completions \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer local" \
  -d '{"model":"phi-4-mini","messages":[{"role":"user","content":"Hello"}]}'

# Streaming
curl http://localhost:5001/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"phi-4-mini","messages":[{"role":"user","content":"Hello"}],"stream":true}'

# Streaming with usage stats
curl http://localhost:5001/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"phi-4-mini","messages":[{"role":"user","content":"Hello"}],"stream":true,"stream_options":{"include_usage":true}}'

# Tool calling
curl http://localhost:5001/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model":"phi-4-mini",
    "messages":[{"role":"user","content":"What is 2*3?"}],
    "tools":[{"type":"function","function":{"name":"multiply","parameters":{"type":"object","properties":{"a":{"type":"number"},"b":{"type":"number"}}}}}],
    "stream":true
  }'
```

## Architecture

```
GitHub Copilot  ──►  Copilot Proxy (ASP.NET Core)  ──►  Foundry Local (native)
   (client)          - SSE streaming fix                  - Model inference
                     - Tool calling fix                    - /v1/* endpoints
                     - finish_reason injection
                     - Usage stats injection
                     - Request sanitization
                     - OpenAI error format
                     - WebSocket realtime
                     - Path normalization
```

## Model Recommendations

For best results with Copilot CLI, choose a model with:
- **Tool calling support** — required for Copilot's agentic features
- **128k+ context window** — Copilot's system prompt + tools use 21k+ tokens
- **Good instruction following** — for accurate code generation

Recommended models:
- `phi-4-mini` — Good balance of quality and speed (default)
- `qwen2.5-0.5b` — Fastest, but limited context and quality
