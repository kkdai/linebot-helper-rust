# ---------- build ----------
FROM rust:1-slim-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config && rm -rf /var/lib/apt/lists/*

# 先只編依賴，讓原始碼變動時可以重用這一層快取
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && echo '' > src/lib.rs \
    && cargo build --release && rm -rf src

COPY src ./src
RUN touch src/main.rs src/lib.rs && cargo build --release

# ---------- runtime ----------
FROM debian:bookworm-slim
# chromium + single-file：爬 JS 渲染頁面；ffmpeg：TTS PCM 轉 m4a
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates chromium ffmpeg nodejs npm \
    && npm install -g single-file-cli \
    && apt-get purge -y npm && apt-get autoremove -y \
    && apt-get clean && rm -rf /var/lib/apt/lists/* /root/.npm

WORKDIR /app
COPY --from=builder /app/target/release/linebot-helper /usr/local/bin/linebot-helper
COPY static ./static

ENV PORT=8080 RUST_LOG=info
EXPOSE 8080
CMD ["linebot-helper"]
