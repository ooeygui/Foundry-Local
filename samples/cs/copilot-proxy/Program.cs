// Copilot Proxy — OpenAI-compatible server wrapping Foundry Local
// Provides SSE streaming, WebSocket realtime API, tool calling support,
// proper finish_reason handling, usage stats, and OpenAI error format.

using System.Net.WebSockets;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

using Microsoft.AI.Foundry.Local;

// ---------------------------------------------------------------------------
// Configuration — supports both FOUNDRY_* and COPILOT_* env vars
// ---------------------------------------------------------------------------
var proxyPort = args.Length > 0 ? args[0] : "5001";
var foundryUrl = Environment.GetEnvironmentVariable("FOUNDRY_URL")
              ?? Environment.GetEnvironmentVariable("COPILOT_PROVIDER_BASE_URL")
              ?? "http://127.0.0.1:5273";
// Strip /v1 suffix if the user set a COPILOT_PROVIDER_BASE_URL that includes it
if (foundryUrl.EndsWith("/v1", StringComparison.OrdinalIgnoreCase))
    foundryUrl = foundryUrl[..^3];

var modelAlias = Environment.GetEnvironmentVariable("FOUNDRY_MODEL")
              ?? Environment.GetEnvironmentVariable("COPILOT_MODEL")
              ?? "phi-4-mini";

// ---------------------------------------------------------------------------
// Initialize Foundry Local
// ---------------------------------------------------------------------------
Console.WriteLine("Initializing Foundry Local...");

var config = new Configuration
{
    AppName = "copilot_proxy",
    LogLevel = Microsoft.AI.Foundry.Local.LogLevel.Information,
    Web = new Configuration.WebService
    {
        Urls = foundryUrl
    }
};

await FoundryLocalManager.CreateAsync(config, Utils.GetAppLogger());
var mgr = FoundryLocalManager.Instance;

// Download and register execution providers (skip on timeout for faster dev startup)
var skipEps = Environment.GetEnvironmentVariable("SKIP_EP_DOWNLOAD") == "1";
if (!skipEps)
{
    var currentEp = "";
    using var epCts = new CancellationTokenSource(TimeSpan.FromSeconds(30));
    try
    {
        await mgr.DownloadAndRegisterEpsAsync((epName, percent) =>
        {
            if (epName != currentEp)
            {
                if (currentEp != "") Console.WriteLine();
                currentEp = epName;
            }
            Console.Write($"\r  {epName.PadRight(30)}  {percent,6:F1}%");
        }, epCts.Token);
        if (currentEp != "") Console.WriteLine();
    }
    catch (OperationCanceledException)
    {
        Console.WriteLine("\n  EP download timed out — continuing with built-in providers.");
    }
    catch (Exception ex)
    {
        Console.WriteLine($"\n  EP download failed ({ex.Message}) — continuing with built-in providers.");
    }
}
else
{
    Console.WriteLine("Skipping EP download (SKIP_EP_DOWNLOAD=1).");
}

// Load model
var catalog = await mgr.GetCatalogAsync();
var model = await catalog.GetModelAsync(modelAlias)
    ?? throw new Exception($"Model '{modelAlias}' not found in catalog");
await model.DownloadAsync(progress =>
{
    Console.Write($"\rDownloading model: {progress:F1}%");
    if (progress >= 100f) Console.WriteLine();
});

Console.Write($"Loading model {model.Id}...");
await model.LoadAsync();
Console.WriteLine("done.");

// Start the native web service (provides the backend we proxy to)
Console.Write($"Starting Foundry web service on {foundryUrl}...");
await mgr.StartWebServiceAsync();
Console.WriteLine("done.");

var actualFoundryUrl = mgr.Urls?.FirstOrDefault() ?? foundryUrl;
Console.WriteLine($"Foundry backend at: {actualFoundryUrl}");

// ---------------------------------------------------------------------------
// Build ASP.NET Core proxy
// ---------------------------------------------------------------------------
var builder = WebApplication.CreateBuilder(args);
builder.WebHost.UseUrls($"http://0.0.0.0:{proxyPort}");

builder.Services.AddHttpClient("foundry", client =>
{
    client.BaseAddress = new Uri(actualFoundryUrl);
    client.Timeout = TimeSpan.FromMinutes(5);
});

var app = builder.Build();

// Enable WebSockets
app.UseWebSockets(new WebSocketOptions
{
    KeepAliveInterval = TimeSpan.FromSeconds(30)
});

// CORS for Copilot
app.UseCors(policy => policy
    .AllowAnyOrigin()
    .AllowAnyMethod()
    .AllowAnyHeader()
    .WithExposedHeaders("X-Request-Id"));

// Global error handler — ensures all errors are in OpenAI format
app.Use(async (ctx, next) =>
{
    try
    {
        await next();
    }
    catch (JsonException ex)
    {
        await WriteOpenAIError(ctx, 400, "invalid_request_error", $"Invalid JSON: {ex.Message}");
    }
    catch (OperationCanceledException)
    {
        // Client disconnected — don't write a response
    }
    catch (Exception ex)
    {
        Console.WriteLine($"[Unhandled error: {ex.Message}]");
        await WriteOpenAIError(ctx, 500, "server_error", "Internal server error");
    }
});

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

// Health check
app.MapGet("/health", () => Results.Ok(new { status = "ok", model = model.Id }));

// Models endpoint
app.MapGet("/v1/models", async (IHttpClientFactory httpFactory) =>
{
    var client = httpFactory.CreateClient("foundry");
    var response = await client.GetAsync("/v1/models");
    var content = await response.Content.ReadAsStringAsync();
    return Results.Content(content, "application/json", statusCode: (int)response.StatusCode);
});

app.MapGet("/v1/models/{modelId}", async (string modelId, IHttpClientFactory httpFactory) =>
{
    var client = httpFactory.CreateClient("foundry");
    var response = await client.GetAsync($"/v1/models/{modelId}");
    var content = await response.Content.ReadAsStringAsync();
    return Results.Content(content, "application/json", statusCode: (int)response.StatusCode);
});

// Chat completions — main endpoint with SSE streaming support
// Supports both /v1/chat/completions (OpenAI standard) and /v1/chat_completions (Foundry native)
app.MapPost("/v1/chat/completions", HandleChatCompletions);
app.MapPost("/v1/chat_completions", HandleChatCompletions);

// Responses API proxy
app.MapPost("/v1/responses", async (HttpContext ctx, IHttpClientFactory httpFactory) =>
{
    var client = httpFactory.CreateClient("foundry");
    var body = await new StreamReader(ctx.Request.Body).ReadToEndAsync();

    var request = new HttpRequestMessage(HttpMethod.Post, "/v1/responses")
    {
        Content = new StringContent(body, Encoding.UTF8, "application/json")
    };

    using var bodyDoc = JsonDocument.Parse(body);
    var isStreaming = bodyDoc.RootElement.TryGetProperty("stream", out var streamProp)
                     && streamProp.GetBoolean();

    if (isStreaming)
    {
        request.Headers.Accept.Add(
            new System.Net.Http.Headers.MediaTypeWithQualityHeaderValue("text/event-stream"));
        var response = await client.SendAsync(request, HttpCompletionOption.ResponseHeadersRead);
        ctx.Response.ContentType = "text/event-stream";
        ctx.Response.Headers.CacheControl = "no-cache";
        ctx.Response.Headers.Connection = "keep-alive";

        await using var stream = await response.Content.ReadAsStreamAsync();
        await stream.CopyToAsync(ctx.Response.Body);
    }
    else
    {
        var response = await client.SendAsync(request);
        var content = await response.Content.ReadAsStringAsync();
        ctx.Response.ContentType = "application/json";
        ctx.Response.StatusCode = (int)response.StatusCode;
        await ctx.Response.WriteAsync(content);
    }
});

// WebSocket endpoint for Realtime API
app.Map("/v1/realtime", async (HttpContext ctx) =>
{
    if (!ctx.WebSockets.IsWebSocketRequest)
    {
        await WriteOpenAIError(ctx, 400, "invalid_request_error", "WebSocket connection required");
        return;
    }

    var ws = await ctx.WebSockets.AcceptWebSocketAsync();
    await HandleRealtimeWebSocket(ws, model.Id);
});

Console.WriteLine($"\n=== Copilot Proxy ready on http://localhost:{proxyPort} ===");
Console.WriteLine($"  Chat completions: POST http://localhost:{proxyPort}/v1/chat/completions");
Console.WriteLine($"  Models:           GET  http://localhost:{proxyPort}/v1/models");
Console.WriteLine($"  Realtime WS:      ws://localhost:{proxyPort}/v1/realtime");
Console.WriteLine($"  Model loaded:     {model.Id}");
Console.WriteLine($"\nCopilot CLI config:");
Console.WriteLine($"  export COPILOT_PROVIDER_BASE_URL=\"http://localhost:{proxyPort}/v1\"");
Console.WriteLine($"  export COPILOT_MODEL=\"{model.Id}\"");
Console.WriteLine();

app.Run();

// ---------------------------------------------------------------------------
// Chat Completions Handler with proper SSE streaming & tool calling
// ---------------------------------------------------------------------------
async Task HandleChatCompletions(HttpContext ctx, IHttpClientFactory httpFactory)
{
    var client = httpFactory.CreateClient("foundry");
    var body = await new StreamReader(ctx.Request.Body).ReadToEndAsync();

    // Check if the client requested usage stats before we strip stream_options
    var wantsUsage = body.Contains("\"include_usage\"") && body.Contains("true");

    // Sanitize the request body — strip fields Foundry may not support
    body = SanitizeRequestBody(body);

    using var bodyDoc = JsonDocument.Parse(body);
    var isStreaming = bodyDoc.RootElement.TryGetProperty("stream", out var streamProp)
                     && streamProp.GetBoolean();

    if (isStreaming)
    {
        await HandleStreamingChatCompletion(ctx, client, body, wantsUsage);
    }
    else
    {
        await HandleNonStreamingChatCompletion(ctx, client, body);
    }
}

async Task HandleNonStreamingChatCompletion(HttpContext ctx, HttpClient client, string body)
{
    var request = new HttpRequestMessage(HttpMethod.Post, "/v1/chat/completions")
    {
        Content = new StringContent(body, Encoding.UTF8, "application/json")
    };

    HttpResponseMessage response;
    try
    {
        response = await client.SendAsync(request, ctx.RequestAborted);
    }
    catch (HttpRequestException ex)
    {
        await WriteOpenAIError(ctx, 502, "upstream_error", $"Foundry backend error: {ex.Message}");
        return;
    }

    var content = await response.Content.ReadAsStringAsync(ctx.RequestAborted);

    if (!response.IsSuccessStatusCode)
    {
        content = WrapAsOpenAIError(content, (int)response.StatusCode);
        ctx.Response.ContentType = "application/json";
        ctx.Response.StatusCode = (int)response.StatusCode;
        await ctx.Response.WriteAsync(content, ctx.RequestAborted);
        return;
    }

    // Fix up the response: ensure finish_reason, add usage if missing
    content = FixNonStreamingResponse(content);

    ctx.Response.ContentType = "application/json";
    ctx.Response.StatusCode = (int)response.StatusCode;
    await ctx.Response.WriteAsync(content, ctx.RequestAborted);
}

async Task HandleStreamingChatCompletion(
    HttpContext ctx, HttpClient client, string body, bool includeUsage)
{
    ctx.Response.ContentType = "text/event-stream";
    ctx.Response.Headers.CacheControl = "no-cache";
    ctx.Response.Headers.Connection = "keep-alive";
    ctx.Response.Headers["X-Accel-Buffering"] = "no";

    var request = new HttpRequestMessage(HttpMethod.Post, "/v1/chat/completions")
    {
        Content = new StringContent(body, Encoding.UTF8, "application/json")
    };

    HttpResponseMessage response;
    try
    {
        response = await client.SendAsync(
            request, HttpCompletionOption.ResponseHeadersRead, ctx.RequestAborted);
    }
    catch
    {
        // Fallback: try underscore path if slash path fails
        var fallbackRequest = new HttpRequestMessage(HttpMethod.Post, "/v1/chat_completions")
        {
            Content = new StringContent(body, Encoding.UTF8, "application/json")
        };
        response = await client.SendAsync(
            fallbackRequest, HttpCompletionOption.ResponseHeadersRead, ctx.RequestAborted);
    }

    if (!response.IsSuccessStatusCode)
    {
        var errorContent = await response.Content.ReadAsStringAsync(ctx.RequestAborted);
        ctx.Response.ContentType = "application/json";
        ctx.Response.StatusCode = (int)response.StatusCode;
        await ctx.Response.WriteAsync(
            WrapAsOpenAIError(errorContent, (int)response.StatusCode), ctx.RequestAborted);
        return;
    }

    await using var stream = await response.Content.ReadAsStreamAsync(ctx.RequestAborted);
    using var reader = new StreamReader(stream);

    var sentDone = false;
    var sawFinishReason = false;
    var sawToolCalls = false;
    var chunkCount = 0;

    while (!reader.EndOfStream && !ctx.RequestAborted.IsCancellationRequested)
    {
        var line = await reader.ReadLineAsync(ctx.RequestAborted);
        if (line == null) break;

        if (line == "data: [DONE]")
        {
            // If usage was requested, inject a final usage chunk before [DONE]
            if (includeUsage)
            {
                var usageChunk = CreateUsageChunk(body, chunkCount);
                await ctx.Response.WriteAsync($"data: {usageChunk}\n\n", ctx.RequestAborted);
            }
            await ctx.Response.WriteAsync("data: [DONE]\n\n", ctx.RequestAborted);
            await ctx.Response.Body.FlushAsync(ctx.RequestAborted);
            sentDone = true;
            break;
        }

        if (line.StartsWith("data: "))
        {
            var jsonData = line["data: ".Length..];
            chunkCount++;

            // Fix up the chunk: handle finish_reason and tool_calls
            jsonData = FixStreamChunk(jsonData, ref sawFinishReason, ref sawToolCalls);

            await ctx.Response.WriteAsync($"data: {jsonData}\n\n", ctx.RequestAborted);
            await ctx.Response.Body.FlushAsync(ctx.RequestAborted);
        }
        else if (!string.IsNullOrEmpty(line))
        {
            await ctx.Response.WriteAsync(line + "\n", ctx.RequestAborted);
            await ctx.Response.Body.FlushAsync(ctx.RequestAborted);
        }
    }

    // If the stream ended without proper termination, fix it up for Copilot
    if (!sentDone && !ctx.RequestAborted.IsCancellationRequested)
    {
        if (!sawFinishReason)
        {
            // Determine the right finish_reason based on whether tool calls were seen
            var reason = sawToolCalls ? "tool_calls" : "stop";
            var finalChunk = CreateFinalChunk(body, reason);
            await ctx.Response.WriteAsync($"data: {finalChunk}\n\n", ctx.RequestAborted);
        }
        if (includeUsage)
        {
            var usageChunk = CreateUsageChunk(body, chunkCount);
            await ctx.Response.WriteAsync($"data: {usageChunk}\n\n", ctx.RequestAborted);
        }
        await ctx.Response.WriteAsync("data: [DONE]\n\n", ctx.RequestAborted);
        await ctx.Response.Body.FlushAsync(ctx.RequestAborted);
    }
}

// ---------------------------------------------------------------------------
// WebSocket Realtime API Handler
// ---------------------------------------------------------------------------
async Task HandleRealtimeWebSocket(WebSocket ws, string modelId)
{
    var buffer = new byte[16384];
    var chatClient = await model.GetChatClientAsync();

    try
    {
        // Send session.created event
        var sessionCreated = JsonSerializer.Serialize(new
        {
            type = "session.created",
            session = new
            {
                id = Guid.NewGuid().ToString(),
                model = modelId,
                modalities = new[] { "text" },
                voice = (string?)null
            }
        });
        await SendWebSocketMessage(ws, sessionCreated);

        while (ws.State == WebSocketState.Open)
        {
            var result = await ws.ReceiveAsync(buffer, CancellationToken.None);

            if (result.MessageType == WebSocketMessageType.Close)
            {
                await ws.CloseAsync(WebSocketCloseStatus.NormalClosure, "Closing", CancellationToken.None);
                break;
            }

            if (result.MessageType == WebSocketMessageType.Text)
            {
                var message = Encoding.UTF8.GetString(buffer, 0, result.Count);
                await HandleRealtimeMessage(ws, chatClient, message, modelId);
            }
        }
    }
    catch (WebSocketException)
    {
        // Client disconnected
    }
}

async Task HandleRealtimeMessage(
    WebSocket ws, OpenAIChatClient chatClient, string message, string modelId)
{
    using var doc = JsonDocument.Parse(message);
    var type = doc.RootElement.GetProperty("type").GetString();

    switch (type)
    {
        case "response.create":
            await HandleRealtimeResponseCreate(ws, chatClient, doc, modelId);
            break;

        case "conversation.item.create":
            var itemCreated = JsonSerializer.Serialize(new
            {
                type = "conversation.item.created",
                item = doc.RootElement.TryGetProperty("item", out var item)
                    ? (object)JsonSerializer.Deserialize<JsonElement>(item.GetRawText())
                    : new { }
            });
            await SendWebSocketMessage(ws, itemCreated);
            break;

        case "session.update":
            var sessionUpdated = JsonSerializer.Serialize(new
            {
                type = "session.updated",
                session = doc.RootElement.TryGetProperty("session", out var session)
                    ? (object)JsonSerializer.Deserialize<JsonElement>(session.GetRawText())
                    : new { }
            });
            await SendWebSocketMessage(ws, sessionUpdated);
            break;

        default:
            var error = JsonSerializer.Serialize(new
            {
                type = "error",
                error = new { message = $"Unknown event type: {type}", code = "unknown_event" }
            });
            await SendWebSocketMessage(ws, error);
            break;
    }
}

async Task HandleRealtimeResponseCreate(
    WebSocket ws, OpenAIChatClient chatClient, JsonDocument doc, string modelId)
{
    var responseId = $"resp_{Guid.NewGuid():N}";
    var itemId = $"item_{Guid.NewGuid():N}";

    await SendWebSocketMessage(ws, JsonSerializer.Serialize(new
    {
        type = "response.created",
        response = new { id = responseId, status = "in_progress", model = modelId }
    }));

    // Extract messages from the request
    var messages = new List<Betalgo.Ranul.OpenAI.ObjectModels.RequestModels.ChatMessage>();

    if (doc.RootElement.TryGetProperty("response", out var responseProp))
    {
        if (responseProp.TryGetProperty("instructions", out var instructions))
        {
            messages.Add(Betalgo.Ranul.OpenAI.ObjectModels.RequestModels.ChatMessage.FromSystem(
                instructions.GetString() ?? "You are a helpful assistant."));
        }

        if (responseProp.TryGetProperty("input", out var inputItems))
        {
            foreach (var inputItem in inputItems.EnumerateArray())
            {
                if (inputItem.TryGetProperty("type", out var itemType)
                    && itemType.GetString() == "message")
                {
                    var role = inputItem.TryGetProperty("role", out var r) ? r.GetString() : "user";
                    if (inputItem.TryGetProperty("content", out var content))
                    {
                        foreach (var part in content.EnumerateArray())
                        {
                            if (part.TryGetProperty("text", out var text))
                            {
                                var textStr = text.GetString() ?? "";
                                if (role == "user")
                                    messages.Add(Betalgo.Ranul.OpenAI.ObjectModels.RequestModels
                                        .ChatMessage.FromUser(textStr));
                                else if (role == "assistant")
                                    messages.Add(Betalgo.Ranul.OpenAI.ObjectModels.RequestModels
                                        .ChatMessage.FromAssistant(textStr));
                            }
                        }
                    }
                }
            }
        }
    }

    if (messages.Count == 0)
    {
        messages.Add(
            Betalgo.Ranul.OpenAI.ObjectModels.RequestModels.ChatMessage.FromUser("Hello"));
    }

    // Stream the response
    var fullText = new StringBuilder();
    try
    {
        await foreach (var chunk in chatClient.CompleteChatStreamingAsync(
                           messages, CancellationToken.None))
        {
            var text = chunk.Choices?.FirstOrDefault()?.Delta?.Content ?? "";
            if (!string.IsNullOrEmpty(text))
            {
                fullText.Append(text);
                await SendWebSocketMessage(ws, JsonSerializer.Serialize(new
                {
                    type = "response.text.delta",
                    response_id = responseId,
                    item_id = itemId,
                    delta = text
                }));
            }
        }
    }
    catch (Exception ex)
    {
        Console.WriteLine($"[WebSocket streaming error: {ex.Message}]");
    }

    // Send completion events
    await SendWebSocketMessage(ws, JsonSerializer.Serialize(new
    {
        type = "response.text.done",
        response_id = responseId,
        item_id = itemId,
        text = fullText.ToString()
    }));

    await SendWebSocketMessage(ws, JsonSerializer.Serialize(new
    {
        type = "response.done",
        response = new
        {
            id = responseId,
            status = "completed",
            model = modelId,
            output = new object[]
            {
                new
                {
                    id = itemId,
                    type = "message",
                    role = "assistant",
                    content = new object[] { new { type = "text", text = fullText.ToString() } }
                }
            }
        }
    }));
}

async Task SendWebSocketMessage(WebSocket ws, string message)
{
    if (ws.State == WebSocketState.Open)
    {
        var bytes = Encoding.UTF8.GetBytes(message);
        await ws.SendAsync(bytes, WebSocketMessageType.Text, true, CancellationToken.None);
    }
}

// ---------------------------------------------------------------------------
// Request sanitization — strip fields Foundry doesn't understand
// ---------------------------------------------------------------------------
string SanitizeRequestBody(string body)
{
    try
    {
        var node = JsonNode.Parse(body);
        if (node is not JsonObject obj) return body;

        var modified = false;

        // Remove stream_options — Foundry may reject this unknown field.
        // We handle include_usage ourselves in the proxy.
        if (obj.ContainsKey("stream_options"))
        {
            obj.Remove("stream_options");
            modified = true;
        }

        // Remove parallel_tool_calls if Foundry doesn't support it
        if (obj.ContainsKey("parallel_tool_calls"))
        {
            obj.Remove("parallel_tool_calls");
            modified = true;
        }

        // Remove service_tier — not relevant for local inference
        if (obj.ContainsKey("service_tier"))
        {
            obj.Remove("service_tier");
            modified = true;
        }

        if (modified)
        {
            return obj.ToJsonString();
        }
    }
    catch
    {
        // If parsing fails, return as-is
    }

    return body;
}

// ---------------------------------------------------------------------------
// Stream chunk fixups — tool calling, finish_reason, usage
// ---------------------------------------------------------------------------

/// <summary>
/// Fix a single SSE streaming chunk:
/// - Ensure finish_reason is not silently null when content is present
/// - Track tool_calls presence for proper finish_reason injection
/// - Preserve tool_calls delta structure for Copilot
/// </summary>
string FixStreamChunk(string json, ref bool sawFinishReason, ref bool sawToolCalls)
{
    try
    {
        using var doc = JsonDocument.Parse(json);
        if (!doc.RootElement.TryGetProperty("choices", out var choices))
            return json;

        foreach (var choice in choices.EnumerateArray())
        {
            // Track if this chunk has tool_calls in the delta
            if (choice.TryGetProperty("delta", out var delta)
                && delta.TryGetProperty("tool_calls", out _))
            {
                sawToolCalls = true;
            }

            // Track finish_reason
            if (choice.TryGetProperty("finish_reason", out var fr)
                && fr.ValueKind != JsonValueKind.Null)
            {
                sawFinishReason = true;

                // If the model said "stop" but we saw tool_calls, fix it to "tool_calls"
                if (sawToolCalls && fr.GetString() == "stop")
                {
                    return RewriteFinishReason(json, "tool_calls");
                }
            }
        }
    }
    catch
    {
        // Pass through unparseable chunks
    }

    return json;
}

/// <summary>
/// Fix a non-streaming response: ensure finish_reason, add usage if missing.
/// </summary>
string FixNonStreamingResponse(string json)
{
    try
    {
        var node = JsonNode.Parse(json);
        if (node is not JsonObject obj) return json;

        var modified = false;

        // Fix finish_reason in choices
        if (obj["choices"] is JsonArray choicesArr)
        {
            foreach (var choice in choicesArr)
            {
                if (choice is not JsonObject choiceObj) continue;

                // Fix null/missing finish_reason
                var hasToolCalls = choiceObj["message"] is JsonObject msgObj
                                   && msgObj.ContainsKey("tool_calls");
                var expectedReason = hasToolCalls ? "tool_calls" : "stop";

                if (!choiceObj.ContainsKey("finish_reason")
                    || choiceObj["finish_reason"] is null
                    || choiceObj["finish_reason"]!.GetValueKind() == JsonValueKind.Null)
                {
                    choiceObj["finish_reason"] = expectedReason;
                    modified = true;
                }

                // If finish_reason is "stop" but there are tool_calls, fix it
                if (choiceObj["finish_reason"]?.GetValue<string>() == "stop" && hasToolCalls)
                {
                    choiceObj["finish_reason"] = "tool_calls";
                    modified = true;
                }
            }
        }

        // Add usage if missing
        if (!obj.ContainsKey("usage") || obj["usage"] is null
            || obj["usage"]!.GetValueKind() == JsonValueKind.Null)
        {
            obj["usage"] = new JsonObject
            {
                ["prompt_tokens"] = 0,
                ["completion_tokens"] = 0,
                ["total_tokens"] = 0
            };
            modified = true;
        }

        if (modified)
        {
            return obj.ToJsonString();
        }
    }
    catch
    {
        // If parsing fails, return as-is
    }

    return json;
}

/// <summary>
/// Rewrite finish_reason in a JSON chunk string.
/// </summary>
string RewriteFinishReason(string json, string newReason)
{
    try
    {
        var node = JsonNode.Parse(json);
        if (node is not JsonObject obj) return json;

        if (obj["choices"] is JsonArray choices)
        {
            foreach (var choice in choices)
            {
                if (choice is JsonObject choiceObj)
                {
                    choiceObj["finish_reason"] = newReason;
                }
            }
        }

        return obj.ToJsonString();
    }
    catch
    {
        return json;
    }
}

// ---------------------------------------------------------------------------
// Chunk creation helpers
// ---------------------------------------------------------------------------
string CreateFinalChunk(string originalRequestBody, string finishReason)
{
    var modelName = ExtractModelName(originalRequestBody);

    return JsonSerializer.Serialize(new
    {
        id = $"chatcmpl-{Guid.NewGuid():N}",
        @object = "chat.completion.chunk",
        created = DateTimeOffset.UtcNow.ToUnixTimeSeconds(),
        model = modelName,
        choices = new object[]
        {
            new
            {
                index = 0,
                delta = new { },
                finish_reason = finishReason
            }
        }
    });
}

string CreateUsageChunk(string originalRequestBody, int completionTokenEstimate)
{
    var modelName = ExtractModelName(originalRequestBody);

    // Estimate token counts from the request/response.
    // Without a real tokenizer, we approximate from character counts.
    var promptTokenEstimate = EstimatePromptTokens(originalRequestBody);

    return JsonSerializer.Serialize(new
    {
        id = $"chatcmpl-{Guid.NewGuid():N}",
        @object = "chat.completion.chunk",
        created = DateTimeOffset.UtcNow.ToUnixTimeSeconds(),
        model = modelName,
        choices = Array.Empty<object>(),
        usage = new
        {
            prompt_tokens = promptTokenEstimate,
            completion_tokens = completionTokenEstimate,
            total_tokens = promptTokenEstimate + completionTokenEstimate
        }
    });
}

// ---------------------------------------------------------------------------
// OpenAI error response format
// ---------------------------------------------------------------------------
async Task WriteOpenAIError(HttpContext ctx, int statusCode, string type, string message)
{
    if (ctx.Response.HasStarted) return;

    ctx.Response.ContentType = "application/json";
    ctx.Response.StatusCode = statusCode;

    var error = JsonSerializer.Serialize(new
    {
        error = new
        {
            message,
            type,
            param = (string?)null,
            code = (string?)null
        }
    });

    await ctx.Response.WriteAsync(error);
}

/// <summary>
/// Wrap an error response from Foundry in OpenAI error format if it isn't already.
/// </summary>
string WrapAsOpenAIError(string content, int statusCode)
{
    try
    {
        using var doc = JsonDocument.Parse(content);
        // Already in OpenAI format?
        if (doc.RootElement.TryGetProperty("error", out var errorObj)
            && errorObj.TryGetProperty("message", out _))
        {
            return content;
        }

        // Extract message from various Foundry error formats
        string message;
        if (doc.RootElement.TryGetProperty("error", out var simpleError)
            && simpleError.ValueKind == JsonValueKind.String)
        {
            message = simpleError.GetString() ?? "Unknown error";
        }
        else if (doc.RootElement.TryGetProperty("detail", out var detail))
        {
            message = detail.GetString() ?? detail.ToString();
        }
        else if (doc.RootElement.TryGetProperty("message", out var msg))
        {
            message = msg.GetString() ?? "Unknown error";
        }
        else
        {
            message = content;
        }

        return JsonSerializer.Serialize(new
        {
            error = new
            {
                message,
                type = statusCode >= 500 ? "server_error" : "invalid_request_error",
                param = (string?)null,
                code = (string?)null
            }
        });
    }
    catch
    {
        // Content isn't even JSON
        return JsonSerializer.Serialize(new
        {
            error = new
            {
                message = content,
                type = "server_error",
                param = (string?)null,
                code = (string?)null
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Utility helpers
// ---------------------------------------------------------------------------
string ExtractModelName(string requestBody)
{
    try
    {
        using var doc = JsonDocument.Parse(requestBody);
        if (doc.RootElement.TryGetProperty("model", out var m))
            return m.GetString() ?? model.Id;
    }
    catch { }

    return model.Id;
}

int EstimatePromptTokens(string requestBody)
{
    // Rough estimate: ~4 chars per token for English text
    try
    {
        using var doc = JsonDocument.Parse(requestBody);
        if (doc.RootElement.TryGetProperty("messages", out var messages))
        {
            var totalChars = 0;
            foreach (var msg in messages.EnumerateArray())
            {
                if (msg.TryGetProperty("content", out var content))
                {
                    if (content.ValueKind == JsonValueKind.String)
                        totalChars += content.GetString()?.Length ?? 0;
                    else
                        totalChars += content.ToString().Length;
                }
            }
            return Math.Max(1, totalChars / 4);
        }
    }
    catch { }

    return 0;
}
