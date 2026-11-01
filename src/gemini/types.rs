//! Gemini generateContent 的 request / response 型別（REST JSON，camelCase）。
//!
//! Tool、responseSchema、speechConfig 等結構多變的欄位直接用 `serde_json::Value`，
//! 透過 `tools::*` 的 helper 產生，不在型別層面窮舉。

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default)]
    pub parts: Vec<Part>,
}

impl Content {
    pub fn user(parts: Vec<Part>) -> Self {
        Self {
            role: Some("user".into()),
            parts,
        }
    }

    pub fn model(parts: Vec<Part>) -> Self {
        Self {
            role: Some("model".into()),
            parts,
        }
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self::user(vec![Part::text(text)])
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<Blob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_data: Option<FileData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable_code: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_execution_result: Option<CodeExecutionResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
}

impl Part {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            ..Default::default()
        }
    }

    pub fn inline(data: &[u8], mime_type: impl Into<String>) -> Self {
        Self {
            inline_data: Some(Blob {
                mime_type: mime_type.into(),
                data: B64.encode(data),
            }),
            ..Default::default()
        }
    }

    pub fn file_uri(uri: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            file_data: Some(FileData {
                file_uri: uri.into(),
                mime_type: Some(mime_type.into()),
            }),
            ..Default::default()
        }
    }

    pub fn is_thought(&self) -> bool {
        self.thought.unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Blob {
    #[serde(default)]
    pub mime_type: String,
    /// base64 字串
    #[serde(default)]
    pub data: String,
}

impl Blob {
    pub fn decode(&self) -> Option<Vec<u8>> {
        B64.decode(&self.data).ok()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileData {
    pub file_uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub response: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodeExecutionResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default)]
    pub output: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_schema: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_modalities: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speech_config: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    pub thinking_budget: i32,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentRequest {
    pub contents: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    /// 只有 Vertex AI 支援；Developer API 送出前會被清掉
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Value>,
}

impl GenerateContentRequest {
    pub fn new(contents: Vec<Content>) -> Self {
        Self {
            contents,
            ..Default::default()
        }
    }

    pub fn text(prompt: impl Into<String>) -> Self {
        Self::new(vec![Content::user_text(prompt)])
    }

    pub fn with_config(mut self, config: GenerationConfig) -> Self {
        self.generation_config = Some(config);
        self
    }

    pub fn with_tools(mut self, tools: Vec<Value>) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn with_tool_config(mut self, tool_config: Value) -> Self {
        self.tool_config = Some(tool_config);
        self
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub prompt_feedback: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    #[serde(default)]
    pub content: Option<Content>,
    #[serde(default)]
    pub finish_reason: Option<String>,
    #[serde(default)]
    pub grounding_metadata: Option<GroundingMetadata>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingMetadata {
    #[serde(default)]
    pub grounding_chunks: Vec<GroundingChunk>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingChunk {
    #[serde(default)]
    pub web: Option<WebSource>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct WebSource {
    #[serde(default)]
    pub uri: String,
    #[serde(default)]
    pub title: String,
}

impl GenerateContentResponse {
    fn parts(&self) -> impl Iterator<Item = &Part> {
        self.candidates
            .first()
            .and_then(|c| c.content.as_ref())
            .map(|c| c.parts.iter())
            .into_iter()
            .flatten()
    }

    /// 等同 google-genai 的 `response.text`：串接第一個 candidate 的文字 part（略過 thought）。
    pub fn text(&self) -> Option<String> {
        let text: String = self
            .parts()
            .filter(|p| !p.is_thought())
            .filter_map(|p| p.text.as_deref())
            .collect();
        (!text.is_empty()).then_some(text)
    }

    pub fn finish_reason(&self) -> Option<&str> {
        self.candidates.first()?.finish_reason.as_deref()
    }

    /// Google Search grounding 引用來源。
    pub fn grounding_sources(&self) -> Vec<WebSource> {
        self.candidates
            .first()
            .and_then(|c| c.grounding_metadata.as_ref())
            .map(|m| {
                m.grounding_chunks
                    .iter()
                    .filter_map(|c| c.web.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 回應中的 inline 圖片（agentic vision 的標註圖）。
    pub fn inline_images(&self) -> Vec<Vec<u8>> {
        self.parts()
            .filter_map(|p| p.inline_data.as_ref())
            .filter(|b| b.mime_type.starts_with("image/"))
            .filter_map(Blob::decode)
            .collect()
    }

    /// 回應中的 inline 音訊 bytes 與 mime type（TTS）。
    pub fn inline_audio(&self) -> Vec<(Vec<u8>, String)> {
        self.parts()
            .filter_map(|p| p.inline_data.as_ref())
            .filter(|b| b.mime_type.starts_with("audio/"))
            .filter_map(|b| b.decode().map(|d| (d, b.mime_type.clone())))
            .collect()
    }

    pub fn all_parts(&self) -> Vec<Part> {
        self.parts().cloned().collect()
    }
}

/// 常用 Tool JSON。
pub mod tools {
    use serde_json::{Value, json};

    pub fn google_search() -> Value {
        json!({ "googleSearch": {} })
    }

    pub fn google_maps() -> Value {
        json!({ "googleMaps": { "enableWidget": false } })
    }

    pub fn code_execution() -> Value {
        json!({ "codeExecution": {} })
    }

    pub fn function_declarations(decls: Vec<Value>) -> Value {
        json!({ "functionDeclarations": decls })
    }

    pub fn lat_lng_config(latitude: f64, longitude: f64, language_code: &str) -> Value {
        json!({
            "retrievalConfig": {
                "latLng": { "latitude": latitude, "longitude": longitude },
                "languageCode": language_code,
            }
        })
    }

    pub fn prebuilt_voice(voice_name: &str) -> Value {
        json!({ "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": voice_name } } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_serializes_camel_case_and_skips_none() {
        let req = GenerateContentRequest::text("hi")
            .with_config(GenerationConfig {
                temperature: Some(0.0),
                max_output_tokens: Some(2048),
                thinking_config: Some(ThinkingConfig { thinking_budget: 0 }),
                ..Default::default()
            })
            .with_tools(vec![tools::google_search()]);
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(
            v,
            json!({
                "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
                "tools": [{"googleSearch": {}}],
                "generationConfig": {
                    "temperature": 0.0,
                    "maxOutputTokens": 2048,
                    "thinkingConfig": {"thinkingBudget": 0}
                }
            })
        );
    }

    #[test]
    fn response_text_skips_thoughts_and_extracts_sources() {
        let resp: GenerateContentResponse = serde_json::from_value(json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "thinking...", "thought": true},
                    {"text": "Hello "},
                    {"text": "world"}
                ]},
                "finishReason": "STOP",
                "groundingMetadata": {"groundingChunks": [
                    {"web": {"uri": "https://a.example", "title": "A"}},
                    {"retrievedContext": {}}
                ]}
            }]
        }))
        .unwrap();
        assert_eq!(resp.text().as_deref(), Some("Hello world"));
        assert_eq!(resp.finish_reason(), Some("STOP"));
        assert_eq!(
            resp.grounding_sources(),
            vec![WebSource {
                uri: "https://a.example".into(),
                title: "A".into()
            }]
        );
    }

    #[test]
    fn inline_images_are_decoded() {
        let part = Part::inline(b"\x89PNG", "image/png");
        let resp = GenerateContentResponse {
            candidates: vec![Candidate {
                content: Some(Content::model(vec![part, Part::text("done")])),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(resp.inline_images(), vec![b"\x89PNG".to_vec()]);
    }

    #[test]
    fn empty_response_has_no_text() {
        let resp: GenerateContentResponse = serde_json::from_value(json!({})).unwrap();
        assert_eq!(resp.text(), None);
    }
}
