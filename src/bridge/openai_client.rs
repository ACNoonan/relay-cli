use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::json;

pub struct OpenAiClient {
    client: Client,
    api_key: String,
}

impl OpenAiClient {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .context("OPENAI_API_KEY is not set; required for bridge GPT review")?;
        Ok(Self {
            client: Client::new(),
            api_key,
        })
    }

    pub async fn stream_chat_completion(
        &self,
        model: &str,
        system_prompt: &str,
        user_prompt: &str,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<String> {
        let body = json!({
            "model": model,
            "stream": true,
            "messages": [
                { "role": "system", "content": system_prompt },
                { "role": "user", "content": user_prompt }
            ]
        });

        let response = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("calling OpenAI Chat Completions API")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            bail!("OpenAI API error ({status}): {text}");
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut output = String::new();

        while let Some(next) = stream.next().await {
            let bytes = next.context("reading OpenAI stream chunk")?;
            let chunk = String::from_utf8_lossy(&bytes);
            buffer.push_str(&chunk);

            while let Some(newline_idx) = buffer.find('\n') {
                let line: String = buffer.drain(..=newline_idx).collect();
                let line = line.trim();
                if !line.starts_with("data: ") {
                    continue;
                }

                let payload = line.trim_start_matches("data: ").trim();
                if payload == "[DONE]" {
                    return Ok(output);
                }

                let value: serde_json::Value = match serde_json::from_str(payload) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                if let Some(text) = value
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(serde_json::Value::as_str)
                {
                    output.push_str(text);
                    on_delta(text.to_string());
                }
            }
        }

        Ok(output)
    }
}
