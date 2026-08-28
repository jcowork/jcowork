# Jcowork - Cloud Multi-User AI Agent

A cloud-native, multi-user shared AI agent built with Rust (axum + tokio) and React, featuring per-user isolation, persistent memory, scheduled reminders, and multi-LLM provider support.

English | [中文](./README_CN.md)

## Architecture Overview

```
┌─────────────────────────────────────────────────┐
│                   Clients                        │
│  (Web UI / API)                                  │
└──────────────────┬──────────────────────────────┘
                   │ WebSocket / REST
┌──────────────────▼──────────────────────────────┐
│           Gateway Layer (axum + tokio)            │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │  Auth &   │ │ Session  │ │  Delivery Router  │ │
│  │  Users    │ │ Manager  │ │  (per-platform)   │ │
│  │  (JWT)   │ │(DashMap) │ │                   │ │
│  └──────────┘ └──────────┘ └──────────────────┘ │
└──────────────────┬──────────────────────────────┘
                   │ mpsc channel per user
┌──────────────────▼──────────────────────────────┐
│        UserActor (per-user tokio task)            │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │  Agent   │ │  Prompt  │ │  Context Engine   │ │
│  │  Loop    │ │  Builder │ │  (Compressor)     │ │
│  └──────────┘ └──────────┘ └──────────────────┘ │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │  Memory  │ │  Skill   │ │  Tool Registry    │ │
│  │  Manager │ │  System  │ │  & Dispatcher     │ │
│  └──────────┘ └──────────┘ └──────────────────┘ │
└──────────────────┬──────────────────────────────┘
                   │
┌──────────────────▼──────────────────────────────┐
│             Storage & External                    │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │  SQLite  │ │  File    │ │  LLM Providers    │ │
│  │ (per-user│ │  Store   │ │ (DeepSeek,Qwen,   │ │
│  │  WAL)    │ │ (sandbox)│ │  Moonshot,Ollama)  │ │
│                            └──────────────────┘ │
└─────────────────────────────────────────────────┘
```

### Multi-User Concurrency Model

Each user gets a dedicated **UserActor** (tokio task) with its own mpsc channel:

- **DashMap<UserId, UserActorHandle>** — lock-free concurrent session lookup
- **Per-user isolation** — separate SQLite DB, workspace, memory, skill store
- **No shared mutable state** across users — zero lock contention
- **Async I/O** — one user's LLM streaming never blocks another
- **Idle eviction** — UserActors shut down after configurable timeout

```
WebSocket connect → JWT lookup → DashMap get/spawn UserActor → forward via mpsc → AgentLoop processes
```

## Project Structure

```
jcowork/
├── Cargo.toml                        # Workspace root
├── crates/
│   ├── jcowork-server/                  # Binary: axum server entry point
│   ├── jcowork-gateway/                 # HTTP + WebSocket + Auth + Sessions
│   ├── jcowork-agent/                   # AgentLoop, PromptBuilder, ContextEngine
│   ├── jcowork-memory/                  # MemoryProvider, BuiltinSQLite, Manager
│   ├── jcowork-skills/                  # Skill CRUD, Patch, Loader
│   ├── jcowork-tools/                   # Tool trait, Registry, 10+ tool impls
│   ├── jcowork-llm/                     # LlmProvider trait, JSON-driven provider config, SSE streaming
│   ├── jcowork-storage/                 # Database, Migrations, FileStore
│   ├── jcowork-cron/                    # Cron scheduler
│   ├── jcowork-desktop/                 # Tauri v2 desktop app (Mac/Windows)
│   ├── jcowork-feishu/                  # Feishu/Lark bot integration
│   └── jcowork-logs/                    # JSONL log writer
├── web/                               # React + Vite frontend
├── providers.json                    # LLM provider & model configuration
├── Makefile
└── .env.example
```

### Crate Dependency Graph

```
jcowork-server
  └── jcowork-gateway
        ├── jcowork-agent
        │     ├── jcowork-llm
        │     ├── jcowork-memory → jcowork-storage
        │     ├── jcowork-skills → jcowork-storage
        │     ├── jcowork-tools → jcowork-memory, jcowork-skills, jcowork-storage
        │     └── jcowork-cron
        ├── jcowork-feishu
        └── jcowork-storage
```

## Core Modules

| Module | Crate | Description |
|--------|-------|-------------|
| Agent Loop | `jcowork-agent::loop` | Per-user AgentLoop instance, mpsc-based streaming |
| Prompt Builder | `jcowork-agent::prompt` | Injection scanning, memory/skill injection |
| Memory Manager | `jcowork-memory::manager` | Provider architecture, per-user SQLite FTS5 + jieba CJK tokenization |
| Context Engine | `jcowork-agent::context` | Compressor trait, protect_head/tail pattern |
| Skill System | `jcowork-skills::manager` | Skill CRUD + patch + frontmatter parsing |
| Tool Registry | `jcowork-tools::registry` | Rust trait objects, `dyn Tool` dispatch |
| Gateway | `jcowork-gateway` | axum REST + WebSocket, DashMap sessions |
| Delegate | `jcowork-agent::delegate` | tokio::spawn sub-agent tasks |
| Cron Scheduler | `jcowork-cron::scheduler` | Per-user cron jobs via `cron` crate |
| Auth | `jcowork-gateway::auth` | JWT + Argon2 (multi-user authentication) |
| Feishu | `jcowork-feishu` | Feishu/Lark bot: event parsing, API client, per-user config |
| Logging | `jcowork-logs` | JSONL daily rotating logs for LLM and tool calls |

## Desktop App (macOS / Windows)

A native desktop application is available for macOS and Windows, powered by [Tauri v2](https://tauri.app/). The desktop app bundles the entire backend + frontend into a single installable package — no Docker, Python, or Node.js required.

**Download pre-built installers:**

| Platform | Format | Download |
|----------|--------|------|
| macOS (Apple Silicon) | `.dmg` | [Jcowork_0.2.6_aarch64.dmg](https://github.com/jcowork/jcowork/releases/download/v0.2.6/Jcowork_0.2.6_aarch64.dmg) |

**macOS installation:**
1. Download and open the `.dmg` file
2. Drag `Jcowork.app` to your Applications folder
3. Launch Jcowork from Applications (or Spotlight)
4. The app starts the backend automatically and opens the chat window

> **Note:** If macOS shows "Jcowork is damaged and can't be opened", run this command in Terminal to remove the quarantine flag:
> ```bash
> xattr -cr /Applications/Jcowork.app
> ```
> This happens because the app is not yet notarized by Apple. After running the command, the app will open normally.

**Windows installation:**
1. Download the `.msi` or `.exe` installer
2. Run the installer and follow the prompts
3. Launch Jcowork from the Start Menu

**Build from source:**

```bash
# Prerequisites: Rust 1.85+, Node.js 20+
cargo install tauri-cli --version "^2"

# 1. Build frontend (required — Tauri bundles web/dist/ into the app)
cd web
npm install
npm run build    # outputs to web/dist/
cd ..

# 2. Build desktop app (run from project root)
cargo tauri build

# Output:
#   target/release/bundle/dmg/Jcowork_0.2.6_aarch64.dmg   (macOS)
#   target/release/bundle/msi/Jcowork_0.2.6_x64.msi        (Windows)
```

> **Important:** Always run `npm run build` in `web/` before `cargo tauri build`. The Tauri bundler copies `web/dist/` into the app bundle — if the dist is stale or missing, the desktop app will show a blank screen.

> **Note:** The desktop app requires at least one LLM API key configured in `.env`. The Docling PDF parsing service is optional and runs separately if available.

**Desktop app architecture:**

The desktop app embeds the entire Axum backend into the same binary as the Tauri frontend. At launch:
1. The Axum server starts on `127.0.0.1:3000` (with a TCP readiness check)
2. The WebView loads the frontend via Tauri's `custom-protocol` (`tauri://localhost`)
3. The frontend auto-detects the Tauri environment and routes all API/WebSocket calls to `http://localhost:3000`

This means no separate server process is needed — everything runs in a single app bundle.

## Quick Start

### Prerequisites

- **Rust** 1.85+ (edition 2024) — `rustup` will auto-install via `rust-toolchain.toml`
- **Node.js** 20+ (for frontend)
- **Python** 3.12+ (for web search & document parsing tools)
- **SQLite** 3.35+ (with FTS5 support, usually built-in)
- An **LLM API key** for at least one provider (DeepSeek, Qwen, Moonshot, OpenRouter, etc.)

<details>
<summary>Ubuntu 24.04 one-liner for system dependencies</summary>

```bash
sudo apt-get update && sudo apt-get install -y build-essential pkg-config libssl-dev python3 python3-venv nodejs npm
```

- `build-essential` + `pkg-config` + `libssl-dev` — required by Rust crates (openssl-sys)
- `python3` + `python3-venv` — Python runtime and venv support
- `nodejs` + `npm` — frontend build

</details>

<details>
<summary>Windows 11 setup notes</summary>

- Install [Python 3.12+](https://www.python.org/downloads/) (check "Add to PATH")
- Install [Node.js 18+](https://nodejs.org/) (includes npm)
- Install [Rust via rustup](https://www.rust-lang.org/tools/install) — the Windows installer includes MSVC build tools
- Or install Visual Studio Build Tools manually: `winget install Microsoft.VisualStudio.2022.BuildTools --override '--add Microsoft.VisualStudio.Workload.VCTools --quiet'`

</details>

### 1. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
# rust-toolchain.toml will auto-select Rust 1.85 when you build
```

### 2. Configure Environment

```bash
cp .env.example .env
# Edit .env and set your API key(s):
#   DEEPSEEK_API_KEY=sk-your-key
#   QWEN_API_KEY=sk-your-key
#   MOONSHOT_API_KEY=sk-your-key
#   # or OPENROUTER_API_KEY=sk-your-key
# Set default model:
#   JCWORK_DEFAULT_MODEL=moonshot:kimi-k2.6
```

### 3. Setup Python Environment (for web search & document parsing)

**Linux / macOS:**
```bash
make setup-python
```

**Windows (PowerShell):**
```powershell
powershell -ExecutionPolicy Bypass -File scripts\setup-python.ps1
```

This creates a Python venv with:
- **playwright** — headless browser for web search (Sogou WAP + Bing fallback)
- **docling** — IBM's document understanding library for PDF to Markdown conversion
- **sentence-transformers** — local embedding model for semantic document search

### 4. Start Docling Service (for document parsing & vector search)

The Docling service is required for PDF document parsing and semantic search. Run it directly with Python:

```bash
# Activate the Python venv (created by make setup-python)
source ~/.jcowork/venv/bin/activate  # Linux/macOS
# or
.\.jcowork\venv\Scripts\activate     # Windows

# Start the Docling service
cd services/docling
python app.py
```

The service will start at `http://localhost:50060`. You should see:
```
Loading Docling converter...
Docling converter loaded.
Loading embedding model: paraphrase-multilingual-MiniLM-L12-v2
Embedding model loaded. Dimension: 384
INFO:     Uvicorn running on http://0.0.0.0:50060
```

#### Verify Docling Service

```bash
# Health check
curl http://localhost:50060/health

# Expected response:
# {"status":"ok","docling_loaded":true,"embedding_loaded":true,"embedding_model":"paraphrase-multilingual-MiniLM-L12-v2","embedding_dim":384}
```

#### Configuration

| Environment Variable | Default | Description |
|---------------------|---------|-------------|
| `DOCLING_SERVICE_URL` | `http://localhost:50060` | Docling service endpoint |
| `EMBEDDING_DIM` | `384` | Embedding vector dimension |
| `EMBEDDING_MODEL` | `paraphrase-multilingual-MiniLM-L12-v2` | Sentence transformers model |

> **Note:** The Docling service downloads the embedding model on first run (~80MB). Subsequent starts are faster as the model is cached.

### 5. Build & Run (Development)

```bash
# Build all crates
make build

# Setup data directory
make setup

# Run the server
make run
# Server starts at http://localhost:3000
```

### 6. Frontend (Development)

The frontend is a React + Vite + TypeScript SPA located in `web/`.

```bash
cd web
npm install
npm run dev
# Dev server starts at http://localhost:5173
# API requests are proxied to the backend at :3000
```

**Available scripts:**

| Command | Description |
|---------|-------------|
| `npm run dev` | Start Vite dev server with hot-reload (port 5173) |
| `npm run build` | Production build → outputs to `web/dist/` |
| `npm run preview` | Preview production build locally |
| `npm run lint` | Run ESLint on source files |

> **Workflow:** During development, run both `make run` (backend on :3000) and `npm run dev` (frontend on :5173) simultaneously. The Vite proxy forwards `/api/*` and `/api/ws` to the backend.
>
> When building the desktop app, you must run `npm run build` first — Tauri bundles `web/dist/` into the final package.

### 7. Verify

```bash
# Health check
curl http://localhost:3000/api/health

# Check registered providers
curl http://localhost:3000/api/providers

# Register a user
curl -X POST http://localhost:3000/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{"username":"testuser","password":"test123"}'

# Login (returns JWT token)
curl -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"testuser","password":"test123"}'
```

## Testing

### Unit Tests

```bash
# Run all unit tests across all crates
make test

# Or with cargo directly:
cargo test --workspace

# Run tests for a specific crate
cargo test -p jcowork-storage
cargo test -p jcowork-memory
cargo test -p jcowork-gateway
```

### Individual Crate Tests

| Crate | What's Tested |
|-------|--------------|
| `jcowork-storage` | Database creation, per-user pools, file operations, path traversal blocking |
| `jcowork-memory` | Memory save/recall/search, FTS5 full-text search |
| `jcowork-skills` | Skill CRUD, patch versioning, frontmatter parsing |
| `jcowork-gateway` | JWT token create/verify, Argon2 password hash/verify |
| `jcowork-agent` | PromptBuilder assembly, injection scanning |
| `jcowork-tools` | ToolRegistry dispatch, schema generation |

### Integration Test

```bash
# Start the server with DeepSeek
DEEPSEEK_API_KEY=sk-test JCWORK_PORT=3001 cargo run --bin jcowork &

# Test the auth flow
TOKEN=$(curl -s -X POST http://localhost:3001/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{"username":"integration","password":"test123"}' | jq -r '.token')

echo "Got token: $TOKEN"

# Test chat endpoint
curl -X POST http://localhost:3001/api/chat \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"message":"Hello, who are you?"}'

# Kill the server
kill %1
```

### WebSocket Test

```bash
# Use websocat or wscat
npm install -g wscat
wscat -c ws://localhost:3000/api/ws/YOUR_USER_ID

# Send a message
> {"content":"Hello agent!"}
```

### Lint & Format

```bash
# Check formatting
make fmt

# Auto-fix formatting
make fmt-fix

# Run clippy lints
make clippy

# Type-check without building
make check
```

## Deployment

### Deployment Workflow

```
1. Edit providers.json    → Add/modify providers and models
2. Edit .env             → Set API keys for providers you want to enable
3. Build backend          → cargo build --release  (or make build)
4. Build frontend         → cd web && npm install && npm run build
5. Deploy                → Copy binary, providers.json, .env, and web/dist/
6. Restart               → Restart the service to pick up changes
```

### Changing Configuration

**To add a new LLM provider or model** (no recompile needed if using external `providers.json`):

1. Edit `providers.json` — add or modify provider entries
2. Add the API key to `.env` (e.g., `NEWPROVIDER_API_KEY=sk-xxx`)
3. Restart the server: `kill $(pgrep -f 'target/release/jcowork') && ./target/release/jcowork &`
4. Verify: `curl http://localhost:3000/api/providers`

**To change API keys or default model**:

1. Edit `.env`
2. Restart the server
3. No rebuild needed

**To change Rust source code**:

1. Edit source files
2. Rebuild: `cargo build --release`
3. Restart the server

### providers.json File Search Order

The server searches for `providers.json` in this order:

1. `./providers.json` — current working directory
2. `./config/providers.json` — config subdirectory
3. `/etc/jcowork/providers.json` — system config (Linux)

The file must exist in one of these locations; the server will fail to start if no `providers.json` is found.

### Option 1: Native Binary

```bash
# Build backend (release)
cargo build --release

# Build frontend (production)
cd web
npm install
npm run build    # Output: web/dist/
cd ..

# Deploy: copy these to your server
scp target/release/jcowork user@server:/opt/jcowork/
scp providers.json user@server:/opt/jcowork/
scp .env user@server:/opt/jcowork/.env
scp -r web/dist user@server:/opt/jcowork/web/dist
```

On the server, you need to start both the **backend** and the **web frontend**:

#### Step 1: Start the backend

```bash
cd /opt/jcowork
./jcowork
# Backend listens on http://0.0.0.0:3000
```

#### Step 2: Serve the web frontend with nginx

Install nginx (if not already installed):

```bash
# Ubuntu/Debian
sudo apt install nginx

# CentOS/RHEL
sudo yum install nginx
```

Create the nginx config file:

```bash
sudo nano /etc/nginx/conf.d/jcowork.conf
```

Paste the following config (see full config in [Reverse Proxy](#reverse-proxy-nginx) below):

```nginx
server {
    listen 80;
    server_name your-domain.com;  # Change to your domain or IP

    root /opt/jcowork/web/dist;
    index index.html;

    location /api/ {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_read_timeout 86400;
    }

    location / {
        try_files $uri $uri/ /index.html;
    }
}
```

Enable and start nginx:

```bash
# Test config
sudo nginx -t

# Start nginx
sudo systemctl enable nginx
sudo systemctl start nginx

# After config changes:
sudo systemctl reload nginx
```

Now visit `http://your-domain.com` in your browser.

#### Frontend Development vs Production

| Mode | Command | URL | How to start |
|------|---------|-----|-------------|
| Development | `cd web && npm run dev` | `http://localhost:5173` | Vite dev server (hot reload, auto-proxies API) |
| Production | `npm run build` + nginx | `http://your-domain/` | nginx serves static files + proxies API |

In development, just run `npm run dev` in the `web/` directory — Vite handles hot reload and proxies `/api` to the backend on port 3000.
In production, the built static files (`web/dist/`) are served by nginx, which also proxies `/api` to the backend.

### Option 2: systemd Service (Linux)

Create `/etc/systemd/system/jcowork.service`:

```ini
[Unit]
Description=Jcowork Agent Server
After=network.target

[Service]
Type=simple
User=jcowork
WorkingDirectory=/opt/jcowork
ExecStart=/opt/jcowork/jcowork
Environment=JCWORK_HOST=0.0.0.0
Environment=JCWORK_PORT=3000
Environment=JCWORK_DATA_DIR=/var/lib/jcowork/data
EnvironmentFile=/etc/jcowork/env
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd -r -s /bin/false jcowork
sudo mkdir -p /var/lib/jcowork/data
sudo chown jcowork:jcowork /var/lib/jcowork/data
sudo cp target/release/jcowork /opt/jcowork/
sudo cp providers.json /opt/jcowork/
sudo cp -r web/dist /opt/jcowork/web/dist
sudo cp .env /etc/jcowork/env
sudo systemctl enable jcowork
sudo systemctl start jcowork

# After config changes:
sudo systemctl restart jcowork

# View logs:
journalctl -u jcowork -f
```

### Reverse Proxy (nginx)

The nginx config serves the frontend static files and proxies API/WebSocket requests to the backend:

```nginx
server {
    listen 80;
    server_name jcowork.example.com;

    # Frontend static files
    root /opt/jcowork/web/dist;
    index index.html;

    # API and WebSocket proxy → backend
    location /api/ {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_read_timeout 86400;  # WebSocket long-lived connections
    }

    # SPA fallback: all other routes → index.html
    location / {
        try_files $uri $uri/ /index.html;
    }
}
```

## Configuration Reference

All config via environment variables:

| Variable | Default | Description |
|----------|---------|-------------|
| `JCWORK_HOST` | `0.0.0.0` | Server bind address |
| `JCWORK_PORT` | `3000` | Server bind port |
| `JCWORK_DATA_DIR` | `~/.jcowork/data` | Data directory for per-user DBs and workspaces |
| `JCWORK_JWT_SECRET` | `change-me-in-production` | JWT signing secret (CHANGE IN PRODUCTION!) |
| `JCWORK_TOKEN_DURATION_HOURS` | `24` | JWT token expiry |
| `JCWORK_DEFAULT_MODEL` | `moonshot:kimi-k2.6` | Default LLM model (`provider:model` format) |
| `DEEPSEEK_API_KEY` | (empty) | DeepSeek API key |
| `QWEN_API_KEY` | (empty) | Qwen / Tongyi Qianwen API key |
| `MOONSHOT_API_KEY` | (empty) | Moonshot / Kimi K2.x API key |
| `OPENROUTER_API_KEY` | (empty) | OpenRouter API key |
| `<PROVIDER>_BASE_URL` | (preset) | Override any provider's base URL |
| `JCWORK_IDLE_TIMEOUT` | `300` | UserActor idle timeout in seconds |

## Supported LLM Providers

All providers use OpenAI-compatible chat completion APIs and share the same streaming infrastructure.
Provider and model definitions are loaded from `providers.json` (see [providers.json](#providersjson-file-search-order)).

| Provider | Env Key | Default Model | Context | Base URL |
|----------|---------|--------------|---------|----------|
| **DeepSeek** | `DEEPSEEK_API_KEY` | `deepseek-v4-flash` | 64K | `api.deepseek.com` |
| **Qwen** | `QWEN_API_KEY` | `qwen3.6-plus` | 131K | `dashscope.aliyuncs.com/compatible-mode/v1` |
| **Moonshot** | `MOONSHOT_API_KEY` | `kimi-k2.5` | 256K | `api.moonshot.cn/v1` |
| OpenRouter | `OPENROUTER_API_KEY` | `anthropic/claude-3.5-sonnet` | 200K | `openrouter.ai/api/v1` |
| Local/Ollama | (none) | `qwen3.5:35b-a3b` | 32K | `localhost:11434/v1` |

### Model Selection

Use the `provider:model` format to select models:

```
deepseek:deepseek-v4-flash   # DeepSeek V4 Flash
qwen:qwen3.6-plus            # Qwen3.6 Plus
moonshot:kimi-k2.6           # Moonshot Kimi K2.6
openrouter:anthropic/claude-3.5-sonnet  # via OpenRouter
local:qwen3.5:35b-a3b        # Local Ollama
```

Providers auto-register when their API key env var is non-empty. The server logs available providers on startup.

### Adding a Custom Provider

To add a new provider, edit `providers.json`:

```json
{
  "id": "my-provider",
  "name": "My Custom Provider",
  "env_key": "MY_PROVIDER_API_KEY",
  "base_url": "https://api.my-provider.com/v1",
  "default_model": "my-model-v1",
  "context_length": 128000,
  "models": [
    { "id": "my-model-v1", "name": "My Model V1", "context_length": 128000 },
    { "id": "my-model-v2", "name": "My Model V2", "context_length": 256000 }
  ]
}
```

Then add the API key to `.env` and restart the server. No recompilation needed.

## Feishu (Lark) Bot Integration

Jcowork supports Feishu as an input channel alongside the web UI. Each user can configure their own Feishu bot, allowing contacts on Feishu to chat with their personal AI agent.

### How It Works

1. Each jcowork user configures their own Feishu Custom App in the **Settings** page
2. When someone sends a message to the bot on Feishu, the event is delivered to Jcowork via webhook
3. Jcowork matches the `app_id` in the event header to find the owning jcowork user
4. The message is processed through the agent loop with full per-user context (memory, skills, tools, reminders)
5. The agent's response is sent back to Feishu as a reply

### Setup Steps

1. **Create a Feishu Custom App** in the [Feishu Developer Console](https://open.feishu.cn/app)
   - Enable the "Bot" capability
   - Under **Permissions**, grant `im:message` (receive messages) and `im:message:send_as_bot` (send messages)

2. **Configure the event subscription URL**
   - In the app's **Event Subscription** page, set the request URL to:
     ```
     https://your-jcowork-domain/api/feishu/event
     ```
   - Subscribe to the event: `im.message.receive_v1`
   - Feishu will send a challenge request to verify the URL — Jcowork handles this automatically using your configured verification token

3. **Configure in Jcowork Settings**
   - Log in to Jcowork web UI
   - Go to **Settings** -> **Feishu Integration**
   - Fill in:
     - **App ID** — from the Feishu Developer Console (e.g., `cli_xxxxx`)
     - **App Secret** — from the Feishu Developer Console
     - **Verification Token** — from the Event Subscription page
     - **Encrypt Key** (optional) — if you enabled event encryption
   - Click **Save**

4. **Start chatting** — Send a message to the bot on Feishu. The agent will respond using your configured model, memory, and skills.

### Per-User Architecture

- Each jcowork user configures **their own** Feishu app — no shared credentials
- Feishu messages are routed to the correct jcowork user by matching `app_id`
- All per-user features work in Feishu: memory, skills, reminders, custom agent identity
- Skill-gated tools (e.g., `web_search`) are available only if the user has enabled the corresponding skill

### API Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/api/feishu/event` | No | Feishu event callback (webhook) |
| GET | `/api/feishu/config` | Yes | Get current user's Feishu config |
| PUT | `/api/feishu/config` | Yes | Save/update Feishu config |
| DELETE | `/api/feishu/config` | Yes | Delete Feishu config |

### Configuration via Web UI

Feishu configuration is managed per-user through the web Settings page. No environment variables are needed.

## Document Indexing & Semantic Search

Jcowork supports automatic document indexing with semantic search capabilities. When you upload PDF or Markdown documents to the workspace, they are automatically parsed and indexed for intelligent retrieval.

### How It Works

1. **Document Upload** — Upload PDF or Markdown files to the Documents page
2. **Automatic Parsing** — PDFs are parsed using [Docling](https://github.com/DS4SD/docling) (IBM's document understanding library) into structured Markdown with tables and images preserved
3. **Vector Indexing** — Document chunks are embedded using a local sentence-transformers model and stored in SQLite for semantic search
4. **Smart Retrieval** — When you ask questions about documents, the system retrieves relevant sections using vector similarity search

### Architecture

```
┌─────────────────────────────────────────────────────────┐
│                   Document Pipeline                       │
├─────────────────────────────────────────────────────────┤
│  PDF Upload → Docling Service → Markdown + Tables       │
│                                    ↓                     │
│                    Document Chunker (by heading/table)   │
│                                    ↓                     │
│              Embedding Service (sentence-transformers)   │
│                                    ↓                     │
│              SQLite Vector Store (BLOB + cosine sim)     │
└─────────────────────────────────────────────────────────┘
```

### Components

| Component | Description |
|-----------|-------------|
| **Docling Service** | Python FastAPI service running on port 50060, handles PDF→Markdown conversion |
| **Embedding Service** | Part of Docling service, generates 384-dim vectors using `paraphrase-multilingual-MiniLM-L12-v2` |
| **Document Chunker** | Rust module that splits Markdown into chunks by headings, tables, and images |
| **Vector Store** | SQLite tables (`doc_chunks`, `chunk_embeddings`) with FTS5 fallback |
| **Doc Retrieve Tool** | Agent tool for semantic search over document chunks |

### Deployment

The Docling service runs as a Python FastAPI server:

```bash
# Activate the Python venv
source ~/.jcowork/venv/bin/activate  # Linux/macOS
.\~\.jcowork\venv\Scripts\activate  # Windows PowerShell

# Start the service
cd services/docling
python app.py
```

The service will be available at `http://localhost:50060`.

### Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `DOCLING_SERVICE_URL` | `http://localhost:50060` | Docling service endpoint |
| `EMBEDDING_DIM` | `384` | Embedding vector dimension |
| `EMBEDDING_MODEL` | `paraphrase-multilingual-MiniLM-L12-v2` | Sentence transformers model |

### Usage

When you attach a document to a chat message, the system automatically:
1. Retrieves the document's full content (up to 15,000 characters)
2. Injects it into the LLM context as reference material
3. The LLM can answer questions directly based on the document content

For larger documents or more precise retrieval, the LLM can also use the `doc_retrieve` tool to perform semantic search over document chunks.

## API Reference

### Authentication

| Method | Path | Description |
|--------|------|-------------|
| POST | `/api/auth/register` | Register new user |
| POST | `/api/auth/login` | Login, returns JWT |

### Chat

| Method | Path | Description |
|--------|------|-------------|
| POST | `/api/chat` | Send message, get response |
| WS | `/api/ws/{user_id}` | WebSocket for streaming chat |

### Data

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/sessions` | List user sessions |
| GET | `/api/skills` | List user skills |
| POST | `/api/skills` | Create a skill |
| GET | `/api/memory` | List user memories |
| GET | `/api/health` | Health check |

## Agent Tools

The Jcowork Agent has built-in tools that the LLM can invoke automatically during conversation. You don't need to use any special syntax — just ask in natural language.

### Reminders (One-Time)

Set a one-time reminder that notifies you at the specified time.

| Tool | Description |
|------|-------------|
| `reminder_add` | Set a one-time reminder at a specific time |
| `reminder_list` | List all your active reminders |
| `reminder_remove` | Cancel a reminder by ID |

**Example:**

```
You: 提醒我下午3点开会
Agent: 🔔 提醒已设置！下午3:00我会提醒你：开会

[At 3:00 PM, you receive a push notification:]
🔔 Reminder: 开会
```

### Cron Jobs (Recurring)

Schedule recurring tasks using standard cron syntax (5-field: minute hour day month weekday).

| Tool | Description |
|------|-------------|
| `cron_add` | Schedule a recurring task |
| `cron_list` | List all your cron jobs |
| `cron_remove` | Remove a cron job by ID |

**Cron schedule examples:**

| Expression | Meaning |
|-----------|---------|
| `0 9 * * *` | Every day at 9:00 AM |
| `0 9 * * 1-5` | Weekdays at 9:00 AM |
| `0 9 * * 1` | Every Monday at 9:00 AM |
| `*/30 * * * *` | Every 30 minutes |
| `0 8,18 * * *` | Every day at 8:00 AM and 6:00 PM |
| `0 9 1 * *` | 1st of every month at 9:00 AM |

**Example:**

```
You: 每天早上9点提醒我写日报
Agent: ✅ Cron job created! Schedule: 0 9 * * * — 每天早上9:00提醒你写日报
```

> **Note:** Reminders and cron jobs are stored in memory. They are lost when the server restarts. Persistent storage will be added in a future release.

## Key Design Decisions

1. **Per-user SQLite** — Each user gets an isolated database file with WAL mode for concurrent reads. No cross-user data leakage.

2. **Actor model** — UserActor pattern via tokio tasks + mpsc channels. No shared mutable state across users means zero lock contention and clean isolation.

3. **Provider architecture** — Memory, LLM, and Context Engine all follow trait-based provider patterns. Swap implementations without touching core logic.

4. **SSE streaming** — LLM responses stream via SSE over HTTP or WebSocket. Tool calls are interleaved with text deltas.

5. **Skill self-improvement** — Agents create skills from experience and patch them during use.

6. **Memory nudges** — Periodic reminders to persist knowledge as declarative facts.

7. **Context compression** — When approaching token limits, older messages are summarized while protecting the system prompt and recent context.

8. **Static binary** — Single binary with no runtime dependencies.

## Technology Stack

| Layer | Technology |
|-------|-----------|
| Language | Rust (edition 2024) |
| Async runtime | tokio |
| Web framework | axum |
| Database | SQLite (sqlx, WAL mode, FTS5) |
| Auth | JWT (jsonwebtoken) + Argon2 |
| LLM client | reqwest + SSE streaming (5 providers) |
| Concurrency | DashMap, mpsc channels |
| Frontend | React + Vite + TypeScript |
| Desktop | Tauri v2 (native macOS/Windows) |

## License

Apache-2.0
