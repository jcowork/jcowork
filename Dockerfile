FROM rust:1.85-bookworm AS builder

WORKDIR /app

# Cache dependencies
COPY Cargo.toml Cargo.lock ./
COPY crates/jcowork-server/Cargo.toml crates/jcowork-server/Cargo.toml
COPY crates/jcowork-gateway/Cargo.toml crates/jcowork-gateway/Cargo.toml
COPY crates/jcowork-agent/Cargo.toml crates/jcowork-agent/Cargo.toml
COPY crates/jcowork-memory/Cargo.toml crates/jcowork-memory/Cargo.toml
COPY crates/jcowork-skills/Cargo.toml crates/jcowork-skills/Cargo.toml
COPY crates/jcowork-tools/Cargo.toml crates/jcowork-tools/Cargo.toml
COPY crates/jcowork-llm/Cargo.toml crates/jcowork-llm/Cargo.toml
COPY crates/jcowork-storage/Cargo.toml crates/jcowork-storage/Cargo.toml
COPY crates/jcowork-cron/Cargo.toml crates/jcowork-cron/Cargo.toml

# Create dummy src files for dependency caching
RUN mkdir -p crates/jcowork-server/src && echo "fn main(){}" > crates/jcowork-server/src/main.rs && \
    mkdir -p crates/jcowork-gateway/src && echo "" > crates/jcowork-gateway/src/lib.rs && \
    mkdir -p crates/jcowork-agent/src && echo "" > crates/jcowork-agent/src/lib.rs && \
    mkdir -p crates/jcowork-memory/src && echo "" > crates/jcowork-memory/src/lib.rs && \
    mkdir -p crates/jcowork-skills/src && echo "" > crates/jcowork-skills/src/lib.rs && \
    mkdir -p crates/jcowork-tools/src && echo "" > crates/jcowork-tools/src/lib.rs && \
    mkdir -p crates/jcowork-llm/src && echo "" > crates/jcowork-llm/src/lib.rs && \
    mkdir -p crates/jcowork-storage/src && echo "" > crates/jcowork-storage/src/lib.rs && \
    mkdir -p crates/jcowork-cron/src && echo "" > crates/jcowork-cron/src/lib.rs

RUN cargo build --release -p jcowork-server 2>/dev/null || true

# Copy actual source code
COPY . .

RUN touch crates/jcowork-server/src/main.rs crates/jcowork-gateway/src/lib.rs \
      crates/jcowork-agent/src/lib.rs crates/jcowork-memory/src/lib.rs \
      crates/jcowork-skills/src/lib.rs crates/jcowork-tools/src/lib.rs \
      crates/jcowork-llm/src/lib.rs crates/jcowork-storage/src/lib.rs \
      crates/jcowork-cron/src/lib.rs

RUN cargo build --release -p jcowork-server

# Frontend builder
FROM node:20-bookworm-slim AS frontend

WORKDIR /app/web

COPY web/package.json web/package-lock.json* ./
RUN npm install

COPY web/ .
RUN npm run build

# Runtime image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/jcowork /usr/local/bin/jcowork
COPY --from=frontend /app/web/dist /opt/jcowork/web/dist
COPY providers.json /opt/jcowork/providers.json

ENV JCWORK_HOST=0.0.0.0
ENV JCWORK_PORT=3000
ENV JCWORK_DATA_DIR=/data

WORKDIR /opt/jcowork

EXPOSE 3000

VOLUME ["/data"]

CMD ["jcowork"]
