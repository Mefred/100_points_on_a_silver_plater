use axum::{
    Json, Router,
    body::Body,
    http::header,
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::fs;
use tower_http::cors::{Any, CorsLayer};

async fn index() -> Html<String> {
    Html(fs::read_to_string("static/index.html").unwrap())
}

async fn ping(Json(body): Json<Value>) -> Response {
    let user_input = body["input"].as_str().unwrap_or("Hello").to_owned();

    let client = reqwest::Client::new();

    let ollama_body = json!({
        "model": "qwen2.5:1.5b",
        "stream": true,
        "messages": [
            {
                "role": "system",
                "content": "You are ResumeBoost AI. Return only resume bullet points."
            },
            {
                "role": "user",
                "content": user_input
            }
        ]
    });

    let ollama_res = match client
        .post("http://127.0.0.1:11434/api/chat")
        .json(&ollama_body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                axum::http::StatusCode::BAD_GATEWAY,
                format!("Ollama error: {e}"),
            )
                .into_response();
        }
    };

    // Forward Ollama's streaming chunks, extracting the text content from each JSON line.
    let stream = ollama_res.bytes_stream().map(|chunk| {
        let chunk = chunk.unwrap_or_default();
        let text = String::from_utf8_lossy(&chunk);
        let mut out = String::new();

        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(content) = v["message"]["content"].as_str() {
                    out.push_str(content);
                }
            }
        }

        Ok::<Bytes, std::io::Error>(Bytes::from(out))
    });

    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            // Tell the browser not to buffer — required for streaming to work.
            (header::TRANSFER_ENCODING, "chunked"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

#[tokio::main]
async fn main() {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(index))
        .route("/ping", post(ping))
        .layer(cors);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();

    println!("Listening on http://127.0.0.1:3000");
    axum::serve(listener, app).await.unwrap();
}
