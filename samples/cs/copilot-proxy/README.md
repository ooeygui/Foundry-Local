# Copilot Proxy — OpenAI-Compatible Server for Foundry Local

An ASP.NET Core proxy server that wraps Foundry Local and provides full OpenAI API compatibility for GitHub Copilot and other tools.

## Features

- **SSE Streaming** — Proper Server-Sent Events streaming for `/v1/chat/completions` with guaranteed `finish_reason` and `[DONE]` termination
- **WebSocket Realtime API** — WebSocket endpoint at `/v1/realtime` implementing the OpenAI Realtime API protocol
- **Path Compatibility** — Supports both `/v1/chat/completions` (OpenAI standard) and `/v1/chat_completions` (Foundry native)
- **finish_reason Fix** — Automatically injects `finish_reason: "stop"` when Foundry omits it, preventing Copilot errors
- **Responses API** — Proxies `/v1/responses` with streaming support
- **CORS** — Wide-open CORS for local development tools

## Why This Exists

Foundry Local's built-in web service has compatibility gaps that cause issues with GitHub Copilot:

1. Streaming responses sometimes omit `finish_reason`, causing Copilot SDK errors
2. No WebSocket support for the Realtime API
3. Streams may not always terminate with `data: [DONE]`

This proxy sits between Copilot and Foundry Local, fixing these issues transparently.

## Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v1/chat/completions` | POST | Chat completions with SSE streaming |
| `/v1/chat_completions` | POST | Alias for Foundry-native path |
| `/v1/models` | GET | List available models |
| `/v1/models/{id}` | GET | Get model details |
| `/v1/responses` | POST | OpenAI Responses API |
| `/v1/realtime` | WebSocket | Realtime API (text modality) |
| `/health` | GET | Health check |

## Usage

```bash
# Default: proxy on port 5001, Foundry on port 5273, phi-4-mini model
dotnet run

# Custom configuration
dotnet run 8080                              # Custom proxy port
FOUNDRY_URL=http://127.0.0.1:6000 dotnet run  # Custom Foundry port
FOUNDRY_MODEL=qwen2.5-0.5b dotnet run        # Different model
```

### Connect GitHub Copilot

Point Copilot at the proxy endpoint:

```typescript
import { CopilotClient } from "@github/copilot-sdk";

const client = new CopilotClient();
const session = await client.createSession({
    model: "phi-4-mini",
    provider: {
        type: "openai",
        baseUrl: "http://localhost:5001/v1",
        apiKey: "local",
        wireApi: "completions",
    },
    streaming: true,
});
```

### Test with curl

```bash
# Non-streaming
curl http://localhost:5001/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"phi-4-mini","messages":[{"role":"user","content":"Hello"}]}'

# Streaming
curl http://localhost:5001/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"phi-4-mini","messages":[{"role":"user","content":"Hello"}],"stream":true}'
```

## Building

```bash
# From the repo root, build the SDK first
dotnet pack sdk/cs/src -o local-packages /p:Version=0.9.0-dev

# Then build the proxy
cd samples/cs/copilot-proxy
dotnet build
```

## Architecture

```
GitHub Copilot  ──►  Copilot Proxy (ASP.NET Core)  ──►  Foundry Local (native)
   (client)          - SSE streaming fix                  - Model inference
                     - WebSocket realtime                  - /v1/* endpoints
                     - finish_reason injection
                     - Path normalization
```
