// Copilot Proxy — OpenAI-compatible server wrapping Foundry Local
// Provides SSE streaming chat completions, WebSocket realtime API, and proper finish_reason handling.

using System.Net.WebSockets;
using System.Text;
using System.Text.Json;

using Microsoft.AI.Foundry.Local;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------
var proxyPort = args.Length > 0 ? args[0] : "5001";
var foundryUrl = Environment.GetEnvironmentVariable("FOUNDRY_URL") ?? "http://127.0.0.1:5273";
var modelAlias = Environment.GetEnvironmentVariable("FOUNDRY_MODEL") ?? "phi-4-mini";

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
var model = await catalog.GetModelAsync(modelAlias) ?? throw new Exception($"Model '{modelAlias}' not found in catalog");
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
    .AllowAnyHeader());

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
    var isStreaming = bodyDoc.RootElement.TryGetProperty("stream", out var streamProp) && streamProp.GetBoolean();

    if (isStreaming)
    {
        request.Headers.Accept.Add(new System.Net.Http.Headers.MediaTypeWithQualityHeaderValue("text/event-stream"));
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
        ctx.Response.StatusCode = 400;
        await ctx.Response.WriteAsync("WebSocket connection required");
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
Console.WriteLine();

app.Run();

// ---------------------------------------------------------------------------
// Chat Completions Handler with proper SSE streaming
// ---------------------------------------------------------------------------
async Task HandleChatCompletions(HttpContext ctx, IHttpClientFactory httpFactory)
{
    var client = httpFactory.CreateClient("foundry");
    var body = await new StreamReader(ctx.Request.Body).ReadToEndAsync();

    using var bodyDoc = JsonDocument.Parse(body);
    var isStreaming = bodyDoc.RootElement.TryGetProperty("stream", out var streamProp) && streamProp.GetBoolean();

    if (isStreaming)
    {
        await HandleStreamingChatCompletion(ctx, client, body);
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

    var response = await client.SendAsync(request);
    var content = await response.Content.ReadAsStringAsync();

    // Ensure finish_reason is present
    content = EnsureFinishReason(content);

    ctx.Response.ContentType = "application/json";
    ctx.Response.StatusCode = (int)response.StatusCode;
    await ctx.Response.WriteAsync(content);
}

async Task HandleStreamingChatCompletion(HttpContext ctx, HttpClient client, string body)
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
        response = await client.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ctx.RequestAborted);
    }
    catch
    {
        // If the backend doesn't support the slash path, try underscore path
        var fallbackRequest = new HttpRequestMessage(HttpMethod.Post, "/v1/chat_completions")
        {
            Content = new StringContent(body, Encoding.UTF8, "application/json")
        };
        response = await client.SendAsync(fallbackRequest, HttpCompletionOption.ResponseHeadersRead, ctx.RequestAborted);
    }

    if (!response.IsSuccessStatusCode)
    {
        var errorContent = await response.Content.ReadAsStringAsync();
        ctx.Response.StatusCode = (int)response.StatusCode;
        ctx.Response.ContentType = "application/json";
        await ctx.Response.WriteAsync(errorContent);
        return;
    }

    await using var stream = await response.Content.ReadAsStreamAsync(ctx.RequestAborted);
    using var reader = new StreamReader(stream);

    var sentDone = false;
    var sawFinishReason = false;

    while (!reader.EndOfStream && !ctx.RequestAborted.IsCancellationRequested)
    {
        var line = await reader.ReadLineAsync(ctx.RequestAborted);
        if (line == null) break;

        if (line == "data: [DONE]")
        {
            await ctx.Response.WriteAsync("data: [DONE]\n\n", ctx.RequestAborted);
            await ctx.Response.Body.FlushAsync(ctx.RequestAborted);
            sentDone = true;
            break;
        }

        if (line.StartsWith("data: "))
        {
            var jsonData = line["data: ".Length..];

            // Check if this chunk contains a finish_reason
            if (jsonData.Contains("\"finish_reason\"") && !jsonData.Contains("\"finish_reason\":null") && !jsonData.Contains("\"finish_reason\": null"))
            {
                sawFinishReason = true;
            }

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
    if (!sentDone)
    {
        if (!sawFinishReason)
        {
            // Send a final chunk with finish_reason: "stop"
            var finalChunk = CreateFinalStopChunk(body);
            await ctx.Response.WriteAsync($"data: {finalChunk}\n\n", ctx.RequestAborted);
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

async Task HandleRealtimeMessage(WebSocket ws, OpenAIChatClient chatClient, string message, string modelId)
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

async Task HandleRealtimeResponseCreate(WebSocket ws, OpenAIChatClient chatClient, JsonDocument doc, string modelId)
{
    var responseId = $"resp_{Guid.NewGuid():N}";
    var itemId = $"item_{Guid.NewGuid():N}";

    // Send response.created
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
                if (inputItem.TryGetProperty("type", out var itemType) && itemType.GetString() == "message")
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
                                    messages.Add(Betalgo.Ranul.OpenAI.ObjectModels.RequestModels.ChatMessage.FromUser(textStr));
                                else if (role == "assistant")
                                    messages.Add(Betalgo.Ranul.OpenAI.ObjectModels.RequestModels.ChatMessage.FromAssistant(textStr));
                            }
                        }
                    }
                }
            }
        }
    }

    if (messages.Count == 0)
    {
        messages.Add(Betalgo.Ranul.OpenAI.ObjectModels.RequestModels.ChatMessage.FromUser("Hello"));
    }

    // Stream the response
    var fullText = new StringBuilder();
    try
    {
        await foreach (var chunk in chatClient.CompleteChatStreamingAsync(messages, CancellationToken.None))
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
// Helpers
// ---------------------------------------------------------------------------
string EnsureFinishReason(string json)
{
    try
    {
        using var doc = JsonDocument.Parse(json);
        if (!doc.RootElement.TryGetProperty("choices", out var choices)) return json;

        var modified = false;
        using var ms = new MemoryStream();
        using (var writer = new Utf8JsonWriter(ms))
        {
            writer.WriteStartObject();
            foreach (var prop in doc.RootElement.EnumerateObject())
            {
                if (prop.Name == "choices")
                {
                    writer.WritePropertyName("choices");
                    writer.WriteStartArray();
                    foreach (var choice in prop.Value.EnumerateArray())
                    {
                        writer.WriteStartObject();
                        var hasFinishReason = false;
                        foreach (var cp in choice.EnumerateObject())
                        {
                            if (cp.Name == "finish_reason")
                            {
                                hasFinishReason = true;
                                if (cp.Value.ValueKind == JsonValueKind.Null)
                                {
                                    writer.WriteString("finish_reason", "stop");
                                    modified = true;
                                }
                                else
                                {
                                    cp.WriteTo(writer);
                                }
                            }
                            else
                            {
                                cp.WriteTo(writer);
                            }
                        }
                        if (!hasFinishReason)
                        {
                            writer.WriteString("finish_reason", "stop");
                            modified = true;
                        }
                        writer.WriteEndObject();
                    }
                    writer.WriteEndArray();
                }
                else
                {
                    prop.WriteTo(writer);
                }
            }
            writer.WriteEndObject();
        }

        if (modified)
        {
            return Encoding.UTF8.GetString(ms.ToArray());
        }
    }
    catch
    {
        // If we can't parse, return as-is
    }

    return json;
}

string CreateFinalStopChunk(string originalRequestBody)
{
    var modelName = "unknown";
    try
    {
        using var doc = JsonDocument.Parse(originalRequestBody);
        if (doc.RootElement.TryGetProperty("model", out var m))
            modelName = m.GetString() ?? "unknown";
    }
    catch { }

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
                finish_reason = "stop"
            }
        }
    });
}
