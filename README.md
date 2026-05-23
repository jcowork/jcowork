# Jcowork - Cloud Multi-User AI Agent

A cloud-native, multi-user shared AI agent built with Rust (axum + tokio) and React, featuring per-user isolation, persistent memory, scheduled reminders, and multi-LLM provider support.

English | [中文](./README_CN.md)

## Architecture Overview

```
┌─────────────────────────────────────────────────┐
│                   Clients                        │
│  (Web UI / CLI / Telegram / Slack / API)         │
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
│  │  WAL)    │ │ (sandbox)│ │  Minimax,Moonshot, │ │
│  └──────────┘ └──────────┘ │  OpenAI,Ollama)   │ │
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
│   └── jcowork-cron/                    # Cron scheduler
├── web/                               # React + Vite frontend
├── providers.json                    # LLM provider & model configuration
├── Dockerfile                         # Multi-stage Rust build
├── docker-compose.yml
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

## Quick Start

### Prerequisites

- **Rust** 1.85+ (edition 2024)
- **Node.js** 18+ (for frontend)
- **SQLite** 3.35+ (with FTS5 support, usually built-in)
- An **LLM API key** for at least one provider (DeepSeek, Qwen, Moonshot, Minimax, OpenAI, etc.)

### 1. Install Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
```

### 2. Configure Environment

```bash
cp .env.example .env
# Edit .env and set your API key(s):
#   DEEPSEEK_API_KEY=sk-your-key
#   QWEN_API_KEY=sk-your-key
#   MOONSHOT_API_KEY=sk-your-key
#   MINIMAX_API_KEY=sk-your-key
#   # or OPENAI_API_KEY=sk-your-key
# Set default model:
#   JCWORK_DEFAULT_MODEL=deepseek:deepseek-chat
```

### 3. Build & Run (Development)

```bash
# Build all crates
make build

# Setup data directory
make setup

# Run the server
make run
# Server starts at http://localhost:3000
```

### 4. Frontend (Development)

```bash
cd web
npm install
npm run dev
# Frontend starts at http://localhost:5173 (proxies API to :3000)
```

### 5. Verify

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
3. Restart the server: `kill $(lsof -ti:3000) && ./target/release/jcowork &`
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

### Option 1: Docker (Recommended for Production)

```bash
# Build the Docker image
make docker

# Start with docker-compose
make docker-up

# Or manually:
docker run -d \
  -p 3000:3000 \
  -v jcowork-data:/data \
  -v $(pwd)/providers.json:/opt/jcowork/providers.json \
  --env-file .env \
  jcowork
```

The Docker image uses a multi-stage build:
1. **Rust builder**: Compiles Rust in release mode
2. **Node builder**: Builds the React frontend
3. **Runtime stage**: Slim image with the binary and static files

Data persists in the `jcowork-data` Docker volume at `/data`.
Mount `providers.json` to customize providers without rebuilding.

The Docker container runs the backend on port 3000.
To serve the web frontend, use nginx on the host with a reverse proxy config (see below),
or add an nginx container to `docker-compose.yml`.

### Option 2: Native Binary

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

### Option 3: systemd Service (Linux)

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
| `JCWORK_DEFAULT_MODEL` | `deepseek:deepseek-chat` | Default LLM model (`provider:model` format) |
| `DEEPSEEK_API_KEY` | (empty) | DeepSeek API key |
| `QWEN_API_KEY` | (empty) | Qwen / Tongyi Qianwen API key |
| `MOONSHOT_API_KEY` | (empty) | Moonshot / Kimi K2.x API key |
| `MINIMAX_API_KEY` | (empty) | Minimax API key |
| `OPENAI_API_KEY` | (empty) | OpenAI API key |
| `OPENROUTER_API_KEY` | (empty) | OpenRouter API key |
| `<PROVIDER>_BASE_URL` | (preset) | Override any provider's base URL |
| `SEARXNG_URL` | `http://localhost:8888` | SearXNG instance for web search |
| `JCWORK_IDLE_TIMEOUT` | `300` | UserActor idle timeout in seconds |

## Supported LLM Providers

All providers use OpenAI-compatible chat completion APIs and share the same streaming infrastructure.
Provider and model definitions are loaded from `providers.json` (see [providers.json](#providersjson-file-search-order)).

| Provider | Env Key | Default Model | Context | Base URL |
|----------|---------|--------------|---------|----------|
| **DeepSeek** | `DEEPSEEK_API_KEY` | `deepseek-chat` | 64K | `api.deepseek.com/v1` |
| **Qwen** | `QWEN_API_KEY` | `qwen-plus` | 131K | `dashscope.aliyuncs.com/compatible-mode/v1` |
| **Moonshot** | `MOONSHOT_API_KEY` | `kimi-k2.5` | 256K | `api.moonshot.cn/v1` |
| **Minimax** | `MINIMAX_API_KEY` | `MiniMax-Text-01` | 1M | `api.minimax.chat/v1` |
| OpenAI | `OPENAI_API_KEY` | `gpt-4o` | 128K | `api.openai.com/v1` |
| OpenRouter | `OPENROUTER_API_KEY` | `anthropic/claude-3.5-sonnet` | 200K | `openrouter.ai/api/v1` |
| Local/Ollama | (none) | `llama3` | 8K | `localhost:11434/v1` |

### Model Selection

Use the `provider:model` format to select models:

```
deepseek:deepseek-chat       # DeepSeek V3
deepseek:deepseek-reasoner   # DeepSeek R1
qwen:qwen-max                # Qwen Max
moonshot:kimi-k2.6           # Moonshot Kimi K2.6
minimax:MiniMax-Text-01      # Minimax 1M context
openai:gpt-4o                # OpenAI GPT-4o
local:llama3                 # Local Ollama
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

8. **Static binary** — Docker build produces a single binary, no runtime dependencies.

## Technology Stack

| Layer | Technology |
|-------|-----------|
| Language | Rust (edition 2024) |
| Async runtime | tokio |
| Web framework | axum |
| Database | SQLite (sqlx, WAL mode, FTS5) |
| Auth | JWT (jsonwebtoken) + Argon2 |
| LLM client | reqwest + SSE streaming (7 providers) |
| Concurrency | DashMap, mpsc channels |
| Frontend | React + Vite + TypeScript |
| Container | Docker multi-stage build |

## License

Apache-2.0
