# linebot-helper-rust

[linebot-helper-python](https://github.com/kkdai/linebot-helper-python) 的 Rust 改寫版：一個以 Gemini 為核心的 LINE 資訊小幫手，部署在 Cloud Run。

功能與 Python 版相同：網址摘要＋社群爆款文案、書籤與研究報告、YouTube／PDF／Office 文件摘要、圖片分析（含 Agentic Vision）、語音訊息轉錄與朗讀、附近地點搜尋與美食深度分析（Gemini Batch API）、多輪對話（Google Search grounding）、LIFF 即時語音助手（Gemini Live API）。

## 架構

```
LINE ──webhook──▶ axum (POST /) ──簽章驗證後立刻回 200──▶ tokio::spawn 背景處理
                                                          │
          ┌───────────────────────────────────────────────┤
          ▼                    ▼                          ▼
   Orchestrator         UrlLoader                   BatchService
   (regex 意圖路由)      (httpx → 瀏覽器 headers      (Files API + batchGenerateContent
     ├ ChatAgent          → SingleFile → Firecrawl    + webhook / 60 秒輪詢)
     ├ Ai (Gemini)        + 文件轉換)
     └ 指令 / 地點
          │
          ▼
   Vertex AI（ADC）／Gemini Developer API（API key）／Firestore REST
```

| 模組 | 說明 | 對應 Python |
|---|---|---|
| `config` | 環境變數（名稱與 Python 版完全相同） | `main.py`、`config/agent_config.py` |
| `gemini` | Gemini REST client：Vertex AI 與 Developer API 共用型別、SSE 串流 | `google-genai` |
| `store` | Firestore REST KV、SessionManager、Bookmark、Report | `services/*_store.py`、`session_manager.py` |
| `line` | LINE API client、webhook 簽章與事件、訊息與 Flex builder | `line-bot-sdk`、`bookmark_flex.py` |
| `loader` | URL fallback chain、HTML→Markdown、文件轉換 | `loader/url.py`、`loader/document.py` |
| `ai` | 摘要、社群文案、研究報告、圖片、語音、TTS、地圖、GitHub（prompt 原文照搬） | `tools/`、`loader/langtools.py` |
| `agents` | Orchestrator 與 ChatAgent | `agents/` |
| `handlers` | webhook、訊息、postback、美食分析、Gemini callback | `main.py` |
| `batch` | Gemini Batch API | `services/batch_service.py` |
| `voice` | LIFF 語音：Gemini Live API WebSocket relay | `services/voice_live.py` |

Prompt 由 `scripts/extract_prompts.py` 用 `ast` 從 Python 原始碼抽出到 `src/ai/prompts/*.txt`，確保一字不差。

## 與 Python 版的差異

- **Google ADK 沒有移植**：Python 版只建立了 ADK Agent 物件，實際路由是 regex，Rust 版直接移植路由邏輯。
- **cloudscraper**：Rust 沒有 Cloudflare JS challenge 解法，改送完整瀏覽器 headers；遇到 challenge 時由 SingleFile（真的 Chromium）接手。
- **anydoc → 自行解析**：PDF（pdf-extract）、docx/pptx/odt/odp/epub（zip + XML）、xlsx/xls/ods（calamine）、csv、rtf。舊二進位格式 `.doc` / `.ppt` 不支援。
- **語音助手的 AI 文字**取自 Live API 的 `outputTranscription`（AUDIO 模式下 model turn 沒有文字 part）。
- **`VOICE_MODEL` 環境變數不使用**：Python 版把模型寫死為 `gemini-3.1-flash-live-preview`，Rust 版維持相同行為。
- **`ENABLE_BATCH_POLLING`**（新增，預設 `true`）：與 Python 版並行期間設為 `false`，避免兩個服務同時輪詢、重複推播同一份美食分析。
- 研究報告頁的標題與網址做了 HTML escape。
- **移除推播端點 `/hn`、`/hf`、`/urls`**（#13）：過去 30 天沒有任何呼叫，且端點沒有驗證，任何人都能觸發爬蟲與推播。`ChannelAccessTokenHF`、`LINE_USER_ID` 因此不再需要。

資料格式（Firestore collections `chat_sessions`、`bookmarks`、`reports`、`batch_jobs`）與 Python 版相同，兩版可共用同一個 Firestore。

## 本機開發

```bash
export ChannelSecret=... ChannelAccessToken=...
export GOOGLE_CLOUD_PROJECT=your-project GOOGLE_AI_API_KEY=...
gcloud auth application-default login   # Vertex AI 與 Firestore 走 ADC

cargo run                                # 監聽 $PORT（預設 8080）
```

缺少必填環境變數時會直接啟動失敗並指出變數名稱。爬 JS 頁面需要 `single-file` 與 Chromium，TTS 需要 `ffmpeg`（Docker image 內都有）。

## 測試

```bash
cargo test                                   # 單元與路由測試（外部服務全部 mock）
cargo clippy --all-targets -- -D warnings

# 需要真實憑證的整合測試
GOOGLE_CLOUD_PROJECT=... GOOGLE_AI_API_KEY=... cargo test -- --ignored --nocapture
```

## 部署到 Cloud Run

```bash
./scripts/deploy.sh
```

1. Cloud Build 建置映像檔（`cloudbuild.yaml`，推到 Artifact Registry `cloud-run-source-deploy`）
2. 從現有的 `linebot-helper-python` 服務複製環境變數與 service account（只寫進暫存檔，不輸出）
3. 部署 `linebot-helper-rust`（us-central1、`--no-cpu-throttling`、`--cpu-boost`、1 CPU / 1Gi、concurrency 10）
4. 執行 `scripts/smoke_test.sh`

> `--no-cpu-throttling` 是必要的：webhook 立刻回 200 後，事件在背景處理，CPU 被凍結的話背景任務不會執行。

### 從 Python 版切換

1. 部署後在 LINE 裡測試前，先確認 smoke test 全部通過。
2. 到 [LINE Developers Console](https://developers.line.biz/console/) 把 Messaging API 的 webhook URL 改成新服務的網址（結尾 `/`），按 Verify。
3. 如有使用 LIFF，把 LIFF endpoint 改成 `https://<新服務>/liff/`。
4. 開啟 Batch 輪詢：`ENABLE_BATCH_POLLING=true ./scripts/deploy.sh`，並停用 Python 版（避免重複輪詢）。

要切回 Python 版，只要把 webhook URL 改回去即可。
