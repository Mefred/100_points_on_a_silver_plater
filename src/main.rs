use axum::{
    Json, Router,
    response::Html,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::fs;
use tower_http::cors::{Any, CorsLayer};

async fn index() -> Html<String> {
    Html(fs::read_to_string("static/index.html").unwrap())
}

async fn ping(Json(body): Json<Value>) -> String {
    let user_input = body["input"].as_str().unwrap_or("Hello");

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

    let mut res = client
        .post("http://127.0.0.1:11434/api/chat")
        .json(&ollama_body)
        .send()
        .await
        .unwrap();

    let mut output = String::new();

    use serde_json::Value as V;

    while let Some(chunk) = res.chunk().await.unwrap() {
        let text = String::from_utf8_lossy(&chunk);

        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<V>(line) {
                if let Some(content) = v["message"]["content"].as_str() {
                    output.push_str(content);
                }
            }
        }
    }

    output
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

    println!("http://127.0.0.1:3000");

    axum::serve(listener, app).await.unwrap();
}
