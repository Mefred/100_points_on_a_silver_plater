use axum::{
    Json, Router,
    body::Body,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower_http::cors::{Any, CorsLayer};

/* ─── Request / Response types ─── */

#[derive(Deserialize)]
struct BoostRequest {
    input: String,
    bullet_count: Option<u8>,
    tone: Option<String>,
    role: Option<String>,
    model: Option<String>,
}

#[derive(Serialize)]
struct BulletResponse {
    bullets: Vec<String>,
}

/* ─── Static file ─── */

async fn index() -> Html<String> {
    Html(
        fs::read_to_string("static/index.html")
            .unwrap_or_else(|_| "<h1>Error: static/index.html not found</h1>".to_string()),
    )
}

/* ─── Shared prompt builder ─── */

fn build_prompt(body: &BoostRequest) -> (String, String) {
    let count = body.bullet_count.unwrap_or(5).clamp(1, 10);
    let tone = body.tone.as_deref().unwrap_or("professional");
    let model = body.model.as_deref().unwrap_or("qwen2.5:1.5b").to_string();

    let tone_instruction = match tone {
        "concise" => "Use very concise, punchy language — each bullet under 15 words.",
        "technical" => "Use technical terminology; highlight specific tools, systems, or methods.",
        "leadership" => "Emphasise leadership, team impact, and cross-functional collaboration.",
        _ => "Use professional resume language appropriate for corporate environments.",
    };

    let role_context = match body.role.as_deref().filter(|r| !r.is_empty()) {
        Some(r) => format!("Target role: {r}\n"),
        None => String::new(),
    };

    let prompt = format!(
        r#"You are an expert resume writer.
{role_context}Return ONLY valid JSON — no markdown, no code fences, no extra text.
Format:
{{
  "bullets": [
    "bullet 1",
    "bullet 2"
  ]
}}

Rules:
- Exactly {count} bullets
- Each bullet MUST start with a strong action verb
- Quantify impact with numbers or percentages where possible
- Tone: {tone_instruction}
- Tailor language to the target role if provided
- No bullet longer than 25 words
- No commentary outside the JSON

Experience to transform:
{input}"#,
        role_context = role_context,
        count = count,
        tone_instruction = tone_instruction,
        input = body.input,
    );

    (prompt, model)
}

/* ─── /stream  — streaming NDJSON endpoint ─── */
//
// The frontend reads this with a ReadableStream / fetch reader.
// Each Ollama chunk arrives as:
//   {"message":{"content":"token"},"done":false}
// We forward each content token as a bare text chunk:
//   data: <token>\n
// and signal completion with:
//   data: [DONE]\n

async fn stream(Json(body): Json<BoostRequest>) -> Response {
    let (prompt, model) = build_prompt(&body);

    let ollama_body = json!({
        "model": model,
        "stream": true,
        "messages": [
            {
                "role": "system",
                "content": "You output ONLY valid JSON. Never include markdown, code fences, or explanation."
            },
            {
                "role": "user",
                "content": prompt
            }
        ]
    });

    let client = reqwest::Client::new();

    let ollama_res = match client
        .post("http://127.0.0.1:11434/api/chat")
        .json(&ollama_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Ollama stream request failed: {e}");
            return (StatusCode::BAD_GATEWAY, e.to_string()).into_response();
        }
    };

    // Channel: Ollama bytes → axum Body
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(64);

    tokio::spawn(async move {
        let mut byte_stream = ollama_res.bytes_stream();

        // Ollama streams newline-delimited JSON; each line is one chunk object.
        // We accumulate partial lines across byte chunks.
        let mut buf = String::new();

        while let Some(chunk) = byte_stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("Stream read error: {e}");
                    break;
                }
            };

            buf.push_str(&String::from_utf8_lossy(&chunk));

            // Process all complete lines
            while let Some(nl) = buf.find('\n') {
                let line = buf[..nl].trim().to_string();
                buf = buf[nl + 1..].to_string();

                if line.is_empty() {
                    continue;
                }

                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    // Extract token from message.content
                    if let Some(token) = v
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        if !token.is_empty() {
                            let msg = format!("data: {}\n", token.replace('\n', "\\n"));
                            let _ = tx.send(Ok(Bytes::from(msg))).await;
                        }
                    }

                    // done flag
                    if v.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                        let _ = tx.send(Ok(Bytes::from("data: [DONE]\n"))).await;
                        return;
                    }
                }
            }
        }

        // Fallback done signal
        let _ = tx.send(Ok(Bytes::from("data: [DONE]\n"))).await;
    });

    let stream = ReceiverStream::new(rx);
    let body = Body::from_stream(stream);

    Response::builder()
        .status(200)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(body)
        .unwrap()
}

/* ─── /ping  — non-streaming (used for single-bullet redo) ─── */

async fn ping(Json(body): Json<BoostRequest>) -> Response {
    let (prompt, model) = build_prompt(&body);

    let client = reqwest::Client::new();

    let ollama_body = json!({
        "model": model,
        "stream": false,
        "messages": [
            {
                "role": "system",
                "content": "You output ONLY valid JSON. Never include markdown, code fences, or explanation."
            },
            {
                "role": "user",
                "content": prompt
            }
        ]
    });

    let res = match client
        .post("http://127.0.0.1:11434/api/chat")
        .json(&ollama_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Ollama ping failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "bullets": [], "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let raw_text = match res.text().await {
        Ok(t) => t,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "bullets": [], "error": "Failed to read response" })),
            )
                .into_response();
        }
    };

    let content: String = serde_json::from_str::<Value>(&raw_text)
        .ok()
        .and_then(|v| {
            v.get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| raw_text.clone());

    let json_str = extract_json(&content);

    let bullets: Vec<String> = serde_json::from_str::<Value>(&json_str)
        .ok()
        .and_then(|v| {
            v.get("bullets").and_then(|b| b.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|i| i.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect()
            })
        })
        .unwrap_or_default();

    (
        [(header::CONTENT_TYPE, "application/json")],
        Json(json!({ "bullets": bullets })),
    )
        .into_response()
}

/* ─── JSON extractor ─── */

fn extract_json(input: &str) -> String {
    let start = input.find('{');
    let end = input.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if e >= s => input[s..=e].to_string(),
        _ => "{}".to_string(),
    }
}

/* ─── /models  — list available Ollama models ─── */

async fn list_models() -> Response {
    let client = reqwest::Client::new();
    let res = match client.get("http://127.0.0.1:11434/api/tags").send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Ollama tags request failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "models": [], "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let raw = match res.json::<Value>().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Ollama tags parse failed: {e}");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "models": [], "error": e.to_string() })),
            )
                .into_response();
        }
    };

    let models: Vec<String> = raw
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    ([(header::CONTENT_TYPE, "application/json")], Json(json!({ "models": models }))).into_response()
}

/* ─── Main ─── */

#[tokio::main]
async fn main() {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(index))
        .route("/ping", post(ping))
        .route("/stream", post(stream))
        .route("/models", get(list_models))
        .layer(cors);

    let addr = "127.0.0.1:3000";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("ResumeBoost AI → http://{addr}");
    axum::serve(listener, app).await.unwrap();
}
