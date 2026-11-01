//! 文字轉語音：Gemini TTS 串流 PCM → ffmpeg 轉 m4a（LINE 語音訊息格式）。

use std::process::Stdio;

use tokio::io::AsyncWriteExt;

use super::Ai;
use crate::gemini::{Content, GenerateContentRequest, GenerationConfig, tools};

pub const TTS_MODEL: &str = "gemini-3.1-flash-tts-preview";
/// Zephyr：明亮、輕快的女聲
pub const TTS_VOICE: &str = "Zephyr";
const DEFAULT_SAMPLE_RATE: u32 = 24000;

/// 從 `audio/L16;codec=pcm;rate=24000` 取出 sample rate。
pub fn parse_sample_rate(mime_type: &str) -> u32 {
    mime_type
        .split(';')
        .filter_map(|p| p.trim().strip_prefix("rate="))
        .find_map(|r| r.parse().ok())
        .unwrap_or(DEFAULT_SAMPLE_RATE)
}

/// s16le 單聲道：每個 sample 2 bytes。
pub fn pcm_duration_ms(pcm_len: usize, sample_rate: u32) -> u64 {
    (pcm_len as f64 / (sample_rate as f64 * 2.0) * 1000.0) as u64
}

/// PCM（s16le、單聲道）→ AAC m4a。透過 stdin/檔案交給 ffmpeg。
pub async fn pcm_to_m4a(pcm: &[u8], sample_rate: u32) -> anyhow::Result<Vec<u8>> {
    // m4a 需要可 seek 的輸出（moov atom），所以輸出寫暫存檔
    let out_path = std::env::temp_dir().join(format!("{}.m4a", uuid::Uuid::new_v4().simple()));
    let mut child = tokio::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "s16le",
            "-ar",
            &sample_rate.to_string(),
            "-ac",
            "1",
            "-i",
            "pipe:0",
            "-c:a",
            "aac",
        ])
        .arg(&out_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to start ffmpeg: {e}"))?;
    let mut stdin = child.stdin.take().expect("stdin piped");
    stdin.write_all(pcm).await?;
    drop(stdin);
    let output = child.wait_with_output().await?;
    let result = if output.status.success() {
        tokio::fs::read(&out_path).await.map_err(Into::into)
    } else {
        Err(anyhow::anyhow!(
            "ffmpeg conversion failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(300)
                .collect::<String>()
        ))
    };
    let _ = tokio::fs::remove_file(&out_path).await;
    result
}

impl Ai {
    /// Gemini TTS 串流，回傳 (PCM s16le bytes, sample rate)。
    pub async fn synthesize_pcm(&self, text: &str) -> anyhow::Result<(Vec<u8>, u32)> {
        let prompt = format!(
            "請使用台灣用語的繁體中文，以親切且自然的語氣朗讀以下摘要內容。## Transcript:\n{text}"
        );
        let req = GenerateContentRequest::new(vec![Content::user_text(prompt)]).with_config(
            GenerationConfig {
                response_modalities: Some(vec!["AUDIO".into()]),
                speech_config: Some(tools::prebuilt_voice(TTS_VOICE)),
                ..Default::default()
            },
        );
        let chunks = self
            .developer
            .stream_generate_content(TTS_MODEL, req)
            .await?;

        let mut pcm = Vec::new();
        let mut sample_rate = DEFAULT_SAMPLE_RATE;
        for chunk in &chunks {
            for (data, mime) in chunk.inline_audio() {
                sample_rate = parse_sample_rate(&mime);
                pcm.extend_from_slice(&data);
            }
        }
        if pcm.is_empty() {
            anyhow::bail!("No audio received from Gemini TTS");
        }
        Ok((pcm, sample_rate))
    }

    /// 回傳 (m4a bytes, 長度 ms)。
    pub async fn text_to_speech(&self, text: &str) -> anyhow::Result<(Vec<u8>, u64)> {
        let (pcm, sample_rate) = self.synthesize_pcm(text).await?;
        let duration_ms = pcm_duration_ms(pcm.len(), sample_rate);
        let m4a = pcm_to_m4a(&pcm, sample_rate).await?;
        tracing::info!(
            "TTS complete: {} bytes, {duration_ms}ms, SR={sample_rate}",
            m4a.len()
        );
        Ok((m4a, duration_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rate_and_duration() {
        assert_eq!(parse_sample_rate("audio/L16;codec=pcm;rate=16000"), 16000);
        assert_eq!(parse_sample_rate("audio/L16"), 24000);
        assert_eq!(pcm_duration_ms(48000, 24000), 1000);
    }

    #[tokio::test]
    async fn ffmpeg_converts_pcm_when_available() {
        if std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_err()
        {
            eprintln!("ffmpeg not installed, skipping");
            return;
        }
        let pcm = vec![0u8; 48000];
        let m4a = pcm_to_m4a(&pcm, 24000).await.unwrap();
        // m4a 檔頭的 ftyp box
        assert_eq!(&m4a[4..8], b"ftyp");
    }
}
