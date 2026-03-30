use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

pub struct SttClient {
    client: Client,
    api_key: String,
    pub model: String,
    endpoint: String,
}

impl SttClient {
    pub fn new(api_key: String, model: String, endpoint: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            endpoint,
        }
    }

    pub async fn transcribe(&self, audio: Vec<u8>, content_type: &str) -> Result<String> {
        let base_mime = content_type.split(';').next().unwrap_or(content_type).trim();
        let filename = mime_to_filename(base_mime);

        let part = reqwest::multipart::Part::bytes(audio)
            .file_name(filename)
            .mime_str(base_mime)
            .context("invalid audio MIME type")?;

        let form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", part);

        let resp = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .context("STT request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("STT API returned {status}: {body}");
        }

        let result: TranscriptionResponse =
            resp.json().await.context("failed to parse STT response")?;
        Ok(result.text)
    }
}

fn mime_to_filename(mime: &str) -> String {
    let ext = match mime {
        "audio/webm" => "webm",
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" => "mp4",
        "audio/ogg" => "ogg",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/wav" | "audio/wave" => "wav",
        "audio/flac" => "flac",
        _ => "webm",
    };
    format!("audio.{ext}")
}
