//! 意圖路由與對話 agent。

pub mod chat;
pub mod orchestrator;

pub use chat::{ChatAgent, ChatReply, ChatSettings, format_chat_response};
pub use orchestrator::{
    AgentResponse, Intent, Orchestrator, OrchestratorResult, detect_intents,
    format_orchestrator_response,
};
