//! LIFF 即時語音助手：瀏覽器 ⇄ 本服務（WebSocket）⇄ Gemini Live API。
//!
//! 移植自 services/voice_live.py 與 main.py 的 `/ws/voice/{session_id}`。
//! Gemini Live 沒有 Rust SDK，這裡直接實作 BidiGenerateContent 的 JSON 協定：
//! - PTT 模式：停用自動 VAD，由瀏覽器送 activity_start / activity_end
//! - 免持模式：保留自動 VAD（可調低靈敏度），不可送 activity 信號
//! - GoAway（連線約 10 分鐘被回收）時帶 session resumption handle 重連

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use base64::Engine;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as GMessage;

use crate::agents::format_orchestrator_response;
use crate::ai::prompts;
use crate::gemini::types::B64;
use crate::line::messages::{text, truncate_reply};
use crate::state::AppState;

/// Python 版寫死的模型（Cloud Run 上的 VOICE_MODEL 環境變數 Python 版並未使用）
pub const VOICE_MODEL: &str = "gemini-3.1-flash-live-preview";
pub const VOICE_NAME: &str = "Aoede";
const LIVE_ENDPOINT: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
const INIT_TIMEOUT: Duration = Duration::from_secs(15);

// ---------- 協定組裝（純函式，可單元測試） ----------

pub fn build_system_instruction(lat: Option<f64>, lng: Option<f64>) -> String {
    // 與 Python 的 `if lat and lng` 相同：0 也視為沒有座標
    let location_info = match (lat, lng) {
        (Some(lat), Some(lng)) if lat != 0.0 && lng != 0.0 => {
            format!("使用者目前位置：緯度 {lat:.6}，經度 {lng:.6}")
        }
        _ => "使用者未提供位置資訊，地點查詢時請請求使用者口述位置".to_string(),
    };
    prompts::fill(prompts::VOICE_SYSTEM, &[("location_info", &location_info)])
}

/// Google Search 與 function declarations 要放在不同的 Tool 物件。
pub fn build_voice_tools() -> Value {
    json!([
        { "googleSearch": {} },
        { "functionDeclarations": [
            {
                "name": "delegate_to_assistant",
                "description": "把需要較長時間的任務（摘要網頁、YouTube 影片、PDF、深入研究問題等）轉交給文字助手背景處理，結果會自動推送到使用者的 LINE 聊天室。呼叫後請口頭告訴使用者：任務已受理，結果稍後會出現在聊天室。",
                "parameters": {
                    "type": "OBJECT",
                    "properties": {
                        "request": { "type": "STRING", "description": "要轉交的完整任務描述（含網址等必要資訊）" }
                    },
                    "required": ["request"]
                }
            },
            {
                "name": "search_nearby_places",
                "description": "查詢使用者目前位置附近的地點（餐廳、停車場、加油站）。使用者的座標由系統自動帶入，不需要提供。",
                "parameters": {
                    "type": "OBJECT",
                    "properties": {
                        "place_type": { "type": "STRING", "enum": ["restaurant", "parking", "gas_station"], "description": "地點類型" },
                        "custom_query": { "type": "STRING", "description": "自訂查詢語句（例如「附近的日式拉麵店」），可省略" }
                    }
                }
            }
        ]}
    ])
}

pub fn build_setup(
    system_instruction: &str,
    handsfree: bool,
    resume_handle: Option<&str>,
    vad_sensitivity: Option<&str>,
) -> Value {
    let mut setup = json!({
        "model": format!("models/{VOICE_MODEL}"),
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": { "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": VOICE_NAME } } },
        },
        "systemInstruction": { "role": "system", "parts": [{ "text": system_instruction }] },
        "tools": build_voice_tools(),
        "inputAudioTranscription": {},
        "outputAudioTranscription": {},
        "sessionResumption": {},
        "contextWindowCompression": { "slidingWindow": {} },
    });
    if let Some(handle) = resume_handle {
        setup["sessionResumption"] = json!({ "handle": handle });
    }
    if !handsfree {
        setup["realtimeInputConfig"] =
            json!({ "automaticActivityDetection": { "disabled": true } });
    } else if vad_sensitivity == Some("low") {
        // 吵雜環境：降低自動 VAD 靈敏度，減少誤觸發／過早切斷
        setup["realtimeInputConfig"] = json!({ "automaticActivityDetection": {
            "startOfSpeechSensitivity": "START_SENSITIVITY_LOW",
            "endOfSpeechSensitivity": "END_SENSITIVITY_LOW",
        }});
    }
    json!({ "setup": setup })
}

/// 把整段語音對話組成單一 LINE 訊息（額度優化：整段只推一則）。
pub fn format_session_summary(turns: &[(String, String)]) -> String {
    if turns.is_empty() {
        return String::new();
    }
    let mut lines = vec![format!("🎤 語音對話摘要（共 {} 輪）", turns.len())];
    for (i, (user, ai)) in turns.iter().enumerate() {
        lines.push(format!("\n{}. 你：{user}\n   🤖：{ai}", i + 1));
    }
    truncate_reply(&lines.join("\n"))
}

// ---------- Session 狀態機 ----------

/// 一段語音 session 的狀態（跨 Gemini 重連保留）。
#[derive(Debug, Default)]
pub struct VoiceSession {
    pub handsfree: bool,
    pub interrupted: bool,
    pub resume_handle: Option<String>,
    pub go_away: bool,
    pub turns: Vec<(String, String)>,
    ai_text: Vec<String>,
    user_text: Vec<String>,
    turn_interrupted: bool,
    saw_tool_call: bool,
}

#[derive(Debug, PartialEq)]
pub enum Outgoing {
    /// 給瀏覽器的 PCM 音訊
    Audio(Vec<u8>),
    /// 給瀏覽器的 JSON 事件
    Event(Value),
    /// 需要執行的 function calls：(id, name, args)
    ToolCalls(Vec<(Option<String>, String, Value)>),
    /// 收到 GoAway，結束此連線並重連
    GoAway,
}

impl VoiceSession {
    pub fn new(handsfree: bool, resume_handle: Option<String>) -> Self {
        Self {
            handsfree,
            resume_handle,
            ..Default::default()
        }
    }

    /// 處理一則 Gemini 伺服器訊息，回傳要做的事。
    pub fn on_server_message(&mut self, msg: &Value) -> Vec<Outgoing> {
        let mut out = Vec::new();

        let sru = &msg["sessionResumptionUpdate"];
        if sru["resumable"].as_bool() == Some(true)
            && let Some(h) = sru["newHandle"].as_str().filter(|h| !h.is_empty())
        {
            self.resume_handle = Some(h.to_string());
        }
        if msg.get("goAway").is_some() {
            self.go_away = true;
            out.push(Outgoing::GoAway);
            return out;
        }

        let sc = &msg["serverContent"];
        let mut model_text = String::new();
        for part in sc["modelTurn"]["parts"].as_array().into_iter().flatten() {
            if let Some(data) = part["inlineData"]["data"].as_str()
                && let Ok(bytes) = B64.decode(data)
            {
                out.push(Outgoing::Audio(bytes));
            }
            if part["thought"].as_bool() != Some(true)
                && let Some(t) = part["text"].as_str()
            {
                model_text.push_str(t);
            }
        }
        // AUDIO 模式下 AI 說的話在 outputTranscription；沒有時才用 modelTurn 的文字
        let ai_chunk = sc["outputTranscription"]["text"]
            .as_str()
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .or_else(|| (!model_text.is_empty()).then_some(model_text));
        if let Some(t) = ai_chunk {
            self.ai_text.push(t.clone());
            out.push(Outgoing::Event(json!({ "type": "text_chunk", "text": t })));
        }

        if let Some(t) = sc["inputTranscription"]["text"]
            .as_str()
            .filter(|t| !t.is_empty())
        {
            self.user_text.push(t.to_string());
            out.push(Outgoing::Event(
                json!({ "type": "user_transcript", "text": self.user_text.concat() }),
            ));
        }

        // 免持 barge-in：Gemini 偵測到使用者插話，前端要清 playback queue
        if sc["interrupted"].as_bool() == Some(true) {
            self.turn_interrupted = true;
            out.push(Outgoing::Event(json!({ "type": "interrupted" })));
        }

        let calls: Vec<(Option<String>, String, Value)> = msg["toolCall"]["functionCalls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|fc| {
                (
                    fc["id"].as_str().map(str::to_string),
                    fc["name"].as_str().unwrap_or_default().to_string(),
                    fc.get("args").cloned().unwrap_or_else(|| json!({})),
                )
            })
            .collect();
        let is_tool_call = !calls.is_empty();
        if is_tool_call {
            self.saw_tool_call = true;
            out.push(Outgoing::ToolCalls(calls));
        }

        if sc["turnComplete"].as_bool() == Some(true) || is_tool_call {
            out.extend(self.end_turn());
        }
        out
    }

    fn end_turn(&mut self) -> Option<Outgoing> {
        let (saw_tool_call, turn_interrupted) = (self.saw_tool_call, self.turn_interrupted);
        self.saw_tool_call = false;
        self.turn_interrupted = false;
        // tool call 造成的 turn 結束不算一輪（模型拿到結果後會接著說）
        if saw_tool_call && self.ai_text.is_empty() {
            return None;
        }
        if turn_interrupted || self.interrupted {
            self.interrupted = false;
            self.ai_text.clear();
            self.user_text.clear();
            return None;
        }
        let user = self.user_text.concat().trim().to_string();
        let ai = self.ai_text.concat().trim().to_string();
        if !ai.is_empty() {
            let user = if user.is_empty() {
                "（語音輸入）".to_string()
            } else {
                user
            };
            self.turns.push((user, ai));
        }
        self.ai_text.clear();
        self.user_text.clear();
        Some(Outgoing::Event(json!({ "type": "turn_complete" })))
    }

    /// 瀏覽器的 JSON 控制事件 → 要送給 Gemini 的訊息。
    pub fn on_browser_event(&mut self, event: &Value) -> Option<Value> {
        match event["type"].as_str()? {
            // PTT：顯式標記說話開始／結束。免持模式交給自動 VAD，不可送 activity。
            "start_of_speech" if !self.handsfree => {
                Some(json!({ "realtimeInput": { "activityStart": {} } }))
            }
            "end_of_speech" if !self.handsfree => {
                Some(json!({ "realtimeInput": { "activityEnd": {} } }))
            }
            "text" => {
                let t = event["text"].as_str().unwrap_or_default().trim();
                (!t.is_empty()).then(|| json!({ "realtimeInput": { "text": t } }))
            }
            "interrupt" => {
                self.interrupted = true;
                None
            }
            _ => None,
        }
    }
}

pub fn audio_input(pcm: &[u8]) -> Value {
    json!({ "realtimeInput": { "audio": { "data": B64.encode(pcm), "mimeType": "audio/pcm;rate=16000" } } })
}

// ---------- HTTP / WebSocket handlers ----------

/// `GET /liff/`：注入 LIFF_ID。
pub async fn serve_liff(State(state): State<Arc<AppState>>) -> Response {
    let path = format!("{}/liff/index.html", state.config.static_dir);
    match tokio::fs::read_to_string(&path).await {
        Ok(html) => Html(html.replace("{{LIFF_ID}}", &state.config.liff_id)).into_response(),
        Err(_) => crate::handlers::detail(StatusCode::NOT_FOUND, "LIFF app not found"),
    }
}

/// `WS /ws/voice/{session_id}`
pub async fn voice_ws(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        tracing::info!("Voice WS connected: {session_id}");
        run_voice_session(state, socket, &session_id).await;
        tracing::info!("Voice WS disconnected: {session_id}");
    })
}

enum BrowserInput {
    Audio(Vec<u8>),
    Event(Value),
}

fn error_event(message: &str) -> WsMessage {
    WsMessage::Text(
        json!({ "type": "error", "message": message })
            .to_string()
            .into(),
    )
}

async fn run_voice_session(state: Arc<AppState>, socket: WebSocket, session_id: &str) {
    let (mut sink, mut stream) = socket.split();

    // 第一則訊息是 init：{user_id, lat, lng, handsfree, vad_sensitivity}
    let init = match tokio::time::timeout(INIT_TIMEOUT, stream.next()).await {
        Ok(Some(Ok(WsMessage::Text(t)))) => serde_json::from_str::<Value>(&t).unwrap_or_default(),
        Ok(_) => return,
        Err(_) => {
            tracing::warn!("Voice WS init timeout: {session_id}");
            let _ = sink.send(error_event("初始化逾時")).await;
            return;
        }
    };
    let user_id = init["user_id"].as_str().unwrap_or(session_id).to_string();
    let (lat, lng) = (init["lat"].as_f64(), init["lng"].as_f64());
    let handsfree = init["handsfree"].as_bool().unwrap_or(false);
    let vad = init["vad_sensitivity"].as_str().map(str::to_string);
    let system_instruction = build_system_instruction(lat, lng);

    // 瀏覽器輸出：單一 writer task；輸入：單一 reader task（跨 Gemini 重連持續存在）
    let (to_browser, mut browser_out) = mpsc::unbounded_channel::<WsMessage>();
    let writer = tokio::spawn(async move {
        while let Some(m) = browser_out.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
    });
    let (browser_tx, mut browser_in) = mpsc::unbounded_channel::<BrowserInput>();
    let reader = tokio::spawn(async move {
        while let Some(Ok(m)) = stream.next().await {
            let input = match m {
                WsMessage::Binary(b) => BrowserInput::Audio(b.to_vec()),
                WsMessage::Text(t) => match serde_json::from_str(&t) {
                    Ok(v) => BrowserInput::Event(v),
                    Err(_) => continue,
                },
                WsMessage::Close(_) => break,
                _ => continue,
            };
            if browser_tx.send(input).is_err() {
                break;
            }
        }
    });

    let resume = state.voice_resume_handles.get(&user_id);
    let mut session = VoiceSession::new(handsfree, resume);

    loop {
        let result = relay_once(
            &state,
            &mut session,
            &mut browser_in,
            &to_browser,
            &system_instruction,
            vad.as_deref(),
            &user_id,
            (lat, lng),
        )
        .await;
        // 保存 handle，瀏覽器斷線重連（15 分鐘內）也能接回對話
        if let Some(h) = &session.resume_handle {
            state
                .voice_resume_handles
                .insert_with_id(&user_id, h.clone());
        }
        if let Err(e) = &result {
            tracing::error!("Voice relay error: {e:#}");
            let _ = to_browser.send(error_event("語音服務暫時無法使用"));
        }
        if result.is_ok() && std::mem::take(&mut session.go_away) {
            tracing::info!("Voice Live GoAway — reconnecting Gemini for {user_id}");
            continue;
        }
        break;
    }

    // session 結束：整段對話推一則彙整（取代每輪一則，節省訊息額度）
    let summary = format_session_summary(&session.turns);
    if !summary.is_empty()
        && let Err(e) = state.line.push(&user_id, vec![text(summary)]).await
    {
        tracing::error!("voice session summary push failed for {user_id}: {e:#}");
    }
    reader.abort();
    drop(to_browser);
    let _ = writer.await;
}

/// 一條 Gemini Live 連線的 relay。回傳 Ok 代表正常結束（瀏覽器離開或 GoAway）。
#[allow(clippy::too_many_arguments)]
async fn relay_once(
    state: &Arc<AppState>,
    session: &mut VoiceSession,
    browser_in: &mut mpsc::UnboundedReceiver<BrowserInput>,
    to_browser: &mpsc::UnboundedSender<WsMessage>,
    system_instruction: &str,
    vad: Option<&str>,
    user_id: &str,
    (lat, lng): (Option<f64>, Option<f64>),
) -> anyhow::Result<()> {
    let url = format!("{LIVE_ENDPOINT}?key={}", state.config.google_ai_api_key);
    let (gemini, _) = tokio_tungstenite::connect_async(url.as_str()).await?;
    let (mut g_tx, mut g_rx) = gemini.split();
    let setup = build_setup(
        system_instruction,
        session.handsfree,
        session.resume_handle.as_deref(),
        vad,
    );
    g_tx.send(GMessage::Text(setup.to_string().into())).await?;

    // 工具在背景執行，結果經 channel 送回，不擋住音訊轉送
    let (tool_tx, mut tool_rx) = mpsc::unbounded_channel::<Value>();

    loop {
        tokio::select! {
            input = browser_in.recv() => {
                let msg = match input {
                    None => return Ok(()), // 瀏覽器斷線
                    Some(BrowserInput::Audio(pcm)) => Some(audio_input(&pcm)),
                    Some(BrowserInput::Event(ev)) => session.on_browser_event(&ev),
                };
                if let Some(m) = msg {
                    g_tx.send(GMessage::Text(m.to_string().into())).await?;
                }
            }
            response = tool_rx.recv() => {
                if let Some(r) = response {
                    g_tx.send(GMessage::Text(r.to_string().into())).await?;
                }
            }
            frame = g_rx.next() => {
                let raw = match frame {
                    None => anyhow::bail!("Gemini Live connection closed"),
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(GMessage::Text(t))) => t.to_string(),
                    Some(Ok(GMessage::Binary(b))) => String::from_utf8_lossy(&b).into_owned(),
                    Some(Ok(GMessage::Close(c))) => anyhow::bail!("Gemini Live closed: {c:?}"),
                    Some(Ok(_)) => continue,
                };
                let Ok(msg) = serde_json::from_str::<Value>(&raw) else { continue };
                if let Some(err) = msg.get("error") {
                    anyhow::bail!("Gemini Live error: {err}");
                }
                for out in session.on_server_message(&msg) {
                    match out {
                        Outgoing::Audio(pcm) => { let _ = to_browser.send(WsMessage::Binary(pcm.into())); }
                        Outgoing::Event(ev) => { let _ = to_browser.send(WsMessage::Text(ev.to_string().into())); }
                        Outgoing::GoAway => return Ok(()),
                        Outgoing::ToolCalls(calls) => {
                            let (state, user_id, tool_tx) = (state.clone(), user_id.to_string(), tool_tx.clone());
                            tokio::spawn(async move {
                                let mut responses = Vec::new();
                                for (id, name, args) in calls {
                                    let result = run_tool(&state, &user_id, lat, lng, &name, &args).await;
                                    responses.push(json!({ "id": id, "name": name, "response": result }));
                                }
                                let _ = tool_tx.send(json!({ "toolResponse": { "functionResponses": responses } }));
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Live function calling 的工具執行器（座標由後端注入）。
pub async fn run_tool(
    state: &Arc<AppState>,
    user_id: &str,
    lat: Option<f64>,
    lng: Option<f64>,
    name: &str,
    args: &Value,
) -> Value {
    match name {
        "delegate_to_assistant" => {
            let request = args["request"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            if request.is_empty() {
                return json!({ "status": "error", "error_message": "目前無法轉交任務" });
            }
            // 不等結果：Live function calling 是同步阻塞，慢任務必須背景跑
            let (state, user_id) = (state.clone(), user_id.to_string());
            tokio::spawn(async move {
                let result = state.orchestrator.process_text(&user_id, &request).await;
                let response = truncate_reply(&format_orchestrator_response(&result));
                let msg = format!("🎤 語音助手轉交的任務完成：\n\n{response}");
                if let Err(e) = state.line.push(&user_id, vec![text(msg)]).await {
                    tracing::error!("voice delegate failed for {user_id}: {e:#}");
                    let _ = state
                        .line
                        .push(
                            &user_id,
                            vec![text("⚠️ 語音助手轉交的任務處理失敗，請稍後再試。")],
                        )
                        .await;
                }
            });
            json!({ "status": "accepted", "message": "任務已受理，結果將推送到 LINE 聊天室" })
        }
        "search_nearby_places" => {
            let (Some(lat), Some(lng)) = (lat, lng) else {
                return json!({
                    "status": "error",
                    "error_message": "沒有使用者座標，請改用 Google Search 或請使用者口述位置",
                });
            };
            let place_type = args["place_type"].as_str().unwrap_or("restaurant");
            match state
                .ai
                .search_nearby_places(lat, lng, place_type, args["custom_query"].as_str())
                .await
            {
                Ok(r) => json!({
                    "status": "success",
                    "places": r.places,
                    "place_type": place_type,
                    "coordinates": { "latitude": lat, "longitude": lng },
                    "restaurant_names": r.restaurant_names,
                }),
                Err(e) => json!({
                    "status": "error",
                    "error_message": format!("無法取得附近地點資訊: {}", e.to_string().chars().take(100).collect::<String>()),
                }),
            }
        }
        other => json!({ "status": "error", "error_message": format!("未知的工具：{other}") }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_for_ptt_disables_vad_and_resumes() {
        let setup = build_setup("sys", false, Some("h1"), None);
        let s = &setup["setup"];
        assert_eq!(s["model"], "models/gemini-3.1-flash-live-preview");
        assert_eq!(
            s["generationConfig"]["responseModalities"],
            json!(["AUDIO"])
        );
        assert_eq!(
            s["realtimeInputConfig"]["automaticActivityDetection"]["disabled"],
            true
        );
        assert_eq!(s["sessionResumption"]["handle"], "h1");
        assert_eq!(s["tools"][0], json!({"googleSearch": {}}));
        assert_eq!(
            s["tools"][1]["functionDeclarations"][0]["name"],
            "delegate_to_assistant"
        );
    }

    #[test]
    fn setup_for_handsfree_vad_modes() {
        let normal = build_setup("sys", true, None, None);
        assert!(normal["setup"].get("realtimeInputConfig").is_none());
        assert_eq!(normal["setup"]["sessionResumption"], json!({}));
        let noisy = build_setup("sys", true, None, Some("low"));
        assert_eq!(
            noisy["setup"]["realtimeInputConfig"]["automaticActivityDetection"]["endOfSpeechSensitivity"],
            "END_SENSITIVITY_LOW"
        );
    }

    #[test]
    fn system_instruction_includes_location() {
        assert!(
            build_system_instruction(Some(25.033), Some(121.5654))
                .contains("緯度 25.033000，經度 121.565400")
        );
        assert!(build_system_instruction(None, None).contains("使用者未提供位置資訊"));
    }

    #[test]
    fn turn_accumulates_transcripts_and_records_summary() {
        let mut s = VoiceSession::new(false, None);
        let audio = B64.encode(b"pcm");
        let out = s.on_server_message(&json!({"serverContent": {
            "modelTurn": {"parts": [{"inlineData": {"data": audio, "mimeType": "audio/pcm;rate=24000"}}]},
            "inputTranscription": {"text": "今天"},
            "outputTranscription": {"text": "晴天"}
        }}));
        assert!(out.contains(&Outgoing::Audio(b"pcm".to_vec())));
        assert!(out.contains(&Outgoing::Event(
            json!({"type": "text_chunk", "text": "晴天"})
        )));
        let out = s
            .on_server_message(&json!({"serverContent": {"inputTranscription": {"text": "天氣"}}}));
        assert_eq!(
            out,
            vec![Outgoing::Event(
                json!({"type": "user_transcript", "text": "今天天氣"})
            )]
        );
        let out = s.on_server_message(&json!({"serverContent": {"turnComplete": true}}));
        assert_eq!(out, vec![Outgoing::Event(json!({"type": "turn_complete"}))]);
        assert_eq!(s.turns, vec![("今天天氣".to_string(), "晴天".to_string())]);
        assert_eq!(
            format_session_summary(&s.turns),
            "🎤 語音對話摘要（共 1 輪）\n\n1. 你：今天天氣\n   🤖：晴天"
        );
    }

    #[test]
    fn tool_call_turn_does_not_count_and_interrupt_clears() {
        let mut s = VoiceSession::new(true, None);
        let out = s.on_server_message(&json!({"toolCall": {"functionCalls": [
            {"id": "c1", "name": "search_nearby_places", "args": {"place_type": "parking"}}
        ]}}));
        assert_eq!(
            out,
            vec![Outgoing::ToolCalls(vec![(
                Some("c1".into()),
                "search_nearby_places".into(),
                json!({"place_type": "parking"})
            )])]
        );
        s.on_server_message(&json!({"serverContent": {"outputTranscription": {"text": "被打斷"}, "interrupted": true}}));
        let out = s.on_server_message(&json!({"serverContent": {"turnComplete": true}}));
        assert!(out.is_empty());
        assert!(s.turns.is_empty());
    }

    #[test]
    fn resumption_and_go_away() {
        let mut s = VoiceSession::new(false, None);
        s.on_server_message(
            &json!({"sessionResumptionUpdate": {"resumable": true, "newHandle": "h2"}}),
        );
        assert_eq!(s.resume_handle.as_deref(), Some("h2"));
        assert_eq!(
            s.on_server_message(&json!({"goAway": {"timeLeft": "10s"}})),
            vec![Outgoing::GoAway]
        );
        assert!(s.go_away);
    }

    #[test]
    fn browser_events_respect_mode() {
        let mut ptt = VoiceSession::new(false, None);
        assert_eq!(
            ptt.on_browser_event(&json!({"type": "start_of_speech"})),
            Some(json!({"realtimeInput": {"activityStart": {}}}))
        );
        let mut hf = VoiceSession::new(true, None);
        assert_eq!(hf.on_browser_event(&json!({"type": "end_of_speech"})), None);
        assert_eq!(
            hf.on_browser_event(&json!({"type": "text", "text": " 你好 "})),
            Some(json!({"realtimeInput": {"text": "你好"}}))
        );
        assert_eq!(hf.on_browser_event(&json!({"type": "interrupt"})), None);
        assert!(hf.interrupted);
        assert_eq!(
            audio_input(b"ab")["realtimeInput"]["audio"]["mimeType"],
            "audio/pcm;rate=16000"
        );
    }
}
