use serde_json::{json, Value};
use std::env;
use futures_util::StreamExt;
use std::io::Write;

type Res<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Clone, Copy)]
enum Provider {
    Anthropic,
    OpenAi,
}

struct Config {
    provider: Provider,
    api_key: String,
    model: String,
    endpoint: String,
}

impl Config {
    fn from_env() -> Res<Self> {
        let provider = match env::var("PROVIDER").as_deref() {
            Ok("openai") => Provider::OpenAi,
            _ => Provider::Anthropic,
        };
        let default_endpoint = match provider {
            Provider::Anthropic => "https://api.anthropic.com/v1/messages",
            Provider::OpenAi => "https://api.openai.com/v1/chat/completions",
        };
        Ok(Self {
            provider,
            api_key: env::var("API_KEY")?,
            model: env::var("MODEL")?,
            endpoint: env::var("ENDPOINT").unwrap_or_else(|_| default_endpoint.into()),
        })
    }
}

async fn ask_stream(
    client: &reqwest::Client,
    cfg: &Config,
    prompt: &str,
    mut on_text: impl FnMut(&str),
) -> Res<String> {
    let messages = json!([{ "role": "user", "content": prompt }]);

    let req = match cfg.provider {
        Provider::Anthropic => client
            .post(&cfg.endpoint)
            .header("x-api-key", &cfg.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model": cfg.model,
                "max_tokens": 1024,
                "stream": true,
                "messages": messages,
            })),
        Provider::OpenAi => client
            .post(&cfg.endpoint)
            .bearer_auth(&cfg.api_key)
            .json(&json!({
                "model": cfg.model,
                "stream": true,
                "messages": messages,
            })),
    };

    let res = req.send().await?;
    let status = res.status();
    if !status.is_success() {
        return Err(format!("{status}: {}", res.text().await?).into());
    }

    let mut stream = res.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut full = String::new();

    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);

        // proses per baris lengkap, sisanya tetap di buffer
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes);
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue; // skip "event: ..." dan baris kosong
            };
            let data = data.trim();
            if data == "[DONE]" {
                return Ok(full);
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else {
                continue;
            };

            let piece = match cfg.provider {
                Provider::Anthropic => match v["type"].as_str() {
                    Some("content_block_delta") if v["delta"]["type"] == "text_delta" => {
                        v["delta"]["text"].as_str()
                    }
                    Some("error") => return Err(format!("stream error: {v}").into()),
                    _ => None,
                },
                Provider::OpenAi => v["choices"][0]["delta"]["content"].as_str(),
            };

            if let Some(t) = piece {
                on_text(t);
                full.push_str(t);
            }
        }
    }
    Ok(full)
}

#[tokio::main]
async fn main() -> Res<()> {
    let cfg = Config::from_env()?;
    let client = reqwest::Client::new();

    ask_stream(&client, &cfg, "Jelasin Rust ownership singkat", |t| {
        print!("{t}");
        std::io::stdout().flush().ok();
    })
    .await?;
    println!();
    Ok(())
}