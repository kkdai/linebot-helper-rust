//! `cargo test --test live_voice -- --ignored --nocapture`（需 GOOGLE_AI_API_KEY）

use futures::{SinkExt, StreamExt};
use linebot_helper::voice::{Outgoing, VoiceSession, build_setup, build_system_instruction};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
#[ignore]
async fn gemini_live_text_turn_returns_audio() {
    linebot_helper::install_crypto_provider();
    let key = std::env::var("GOOGLE_AI_API_KEY").unwrap();
    let url = format!(
        "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={key}"
    );
    let (ws, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .unwrap();
    let (mut tx, mut rx) = ws.split();
    let setup = build_setup(
        &build_system_instruction(Some(25.03), Some(121.56)),
        true,
        None,
        None,
    );
    tx.send(Message::Text(setup.to_string().into()))
        .await
        .unwrap();

    let mut session = VoiceSession::new(true, None);
    let mut sent_text = false;
    let mut audio_bytes = 0;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while let Ok(Some(frame)) = tokio::time::timeout_at(deadline, rx.next()).await {
        let raw = match frame.unwrap() {
            Message::Text(t) => t.to_string(),
            Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
            Message::Close(c) => panic!("closed: {c:?}"),
            _ => continue,
        };
        let msg: Value = serde_json::from_str(&raw).unwrap();
        if msg.get("setupComplete").is_some() && !sent_text {
            println!("setupComplete");
            tx.send(Message::Text(
                json!({"realtimeInput": {"text": "用一句話跟我打招呼"}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            sent_text = true;
            continue;
        }
        for out in session.on_server_message(&msg) {
            match out {
                Outgoing::Audio(a) => audio_bytes += a.len(),
                Outgoing::Event(e) if e["type"] == "turn_complete" => {
                    println!("audio bytes: {audio_bytes}, turns: {:?}", session.turns);
                    assert!(audio_bytes > 0);
                    return;
                }
                Outgoing::Event(e) => println!("event: {e}"),
                other => println!("{other:?}"),
            }
        }
    }
    panic!("no turn_complete within 30s (audio bytes {audio_bytes})");
}
