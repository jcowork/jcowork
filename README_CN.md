# Jcowork - 云端多人共用 AI Agent

基于 Rust（axum + tokio）和 React 构建的云原生、多人共用 AI Agent，支持严格的用户隔离、持久化记忆、定时提醒及多 LLM 提供者。

[English](./README.md) | 中文

## 架构总览

```
┌─────────────────────────────────────────────────┐
│                   客户端                          │
│  (Web UI / API)                                  │
└──────────────────┬──────────────────────────────┘
                   │ WebSocket / REST
┌──────────────────▼──────────────────────────────┐
│           网关层 (axum + tokio)                    │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │ 认证 &   │ │ 会话     │ │  消息分发路由     │ │
│  │ 用户管理 │ │ 管理器   │ │  (多平台适配)     │ │
│  │  (JWT)  │ │(DashMap) │ │                   │ │
│  └──────────┘ └──────────┘ └──────────────────┘ │
└──────────────────┬──────────────────────────────┘
                   │ 每用户独立 mpsc channel
┌──────────────────▼──────────────────────────────┐
│        UserActor（每用户独立 tokio 任务）           │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │ Agent    │ │ Prompt   │ │  上下文引擎       │ │
│  │ Loop     │ │ Builder  │ │  (压缩器)         │ │
│  └──────────┘ └──────────┘ └──────────────────┘ │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │ 记忆     │ │ 技能     │ │  工具注册表       │ │
│  │ 管理器   │ │ 系统     │ │  & 分发器         │ │
│  └──────────┘ └──────────┘ └──────────────────┘ │
└──────────────────┬──────────────────────────────┘
                   │
┌──────────────────▼──────────────────────────────┐
│             存储与外部服务                          │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │  SQLite  │ │  文件    │ │  LLM 提供者       │ │
│  │ (每用户  │ │  存储    │ │ (DeepSeek,Qwen,   │ │
│  │ 独立DB) │ │ (沙箱化) │ │  Moonshot,Ollama)  │ │
│  └──────────┘ └──────────┘ │
└─────────────────────────────────────────────────┘
```

### 多用户并发模型

每个用户拥有一个独立的 **UserActor**（tokio 任务）和专属 mpsc 通道：

- **DashMap<UserId, UserActorHandle>** — 无锁并发会话查找
- **每用户隔离** — 独立的 SQLite 数据库、工作空间、记忆存储、技能库
- **无跨用户共享可变状态** — 零锁竞争
- **异步 I/O** — 某用户的 LLM 流式响应不会阻塞其他用户
- **空闲回收** — UserActor 在超时后自动关闭释放资源

```
WebSocket 连接 → JWT 验证 → DashMap 查找/创建 UserActor → 通过 mpsc 转发消息 → AgentLoop 处理
```

## 项目结构

```
jcowork/
├── Cargo.toml                        # Workspace 根配置
├── crates/
│   ├── jcowork-server/                  # 可执行文件：axum 服务器入口
│   ├── jcowork-gateway/                 # HTTP + WebSocket + 认证 + 会话管理
│   ├── jcowork-agent/                   # AgentLoop、PromptBuilder、ContextEngine
│   ├── jcowork-memory/                  # MemoryProvider、BuiltinSQLite、Manager
│   ├── jcowork-skills/                  # 技能 CRUD、Patch、Loader
│   ├── jcowork-tools/                   # Tool trait、注册表、10+ 工具实现
│   ├── jcowork-llm/                     # LlmProvider trait、JSON 驱动配置、SSE 流式
│   ├── jcowork-storage/                 # 数据库、迁移、文件存储
│   ├── jcowork-cron/                    # 定时任务调度器
│   ├── jcowork-desktop/                 # Tauri v2 桌面应用（Mac/Windows）
│   ├── jcowork-feishu/                  # 飞书/LocalizedMessage机器人集成
│   └── jcowork-logs/                    # JSONL 日志写入器
├── web/                               # React + Vite 前端
├── providers.json                    # LLM 提供者与模型配置
├── Makefile
└── .env.example
```

### Crate 依赖关系

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

## 核心模块

| 模块 | Crate | 说明 |
|------|-------|------|
| Agent Loop | `jcowork-agent::loop` | 每用户独立 AgentLoop 实例，mpsc 流式输出 |
| Prompt Builder | `jcowork-agent::prompt` | 注入扫描，记忆/技能注入 |
| Memory Manager | `jcowork-memory::manager` | Provider 架构，每用户 SQLite FTS5 + jieba 中文分词 |
| Context Engine | `jcowork-agent::context` | Compressor trait，保护首尾消息模式 |
| Skill System | `jcowork-skills::manager` | 技能 CRUD + patch + frontmatter 解析 |
| Tool Registry | `jcowork-tools::registry` | Rust trait objects，`dyn Tool` 分发 |
| Gateway | `jcowork-gateway` | axum REST + WebSocket，DashMap 会话 |
| Delegate | `jcowork-agent::delegate` | tokio::spawn 子 Agent 任务 |
| Cron Scheduler | `jcowork-cron::scheduler` | 每用户定时任务，基于 `cron` crate |
| Auth | `jcowork-gateway::auth` | JWT + Argon2（多用户认证） |

## 桌面应用（macOS / Windows）

基于 [Tauri v2](https://tauri.app/) 构建的原生桌面应用，将完整的后端 + 前端打包为单一可安装程序 — 无需 Docker、Python 或 Node.js。

**下载安装包：**

| 平台 | 格式 | 下载 |
|------|------|------|
| macOS (Apple Silicon) | `.dmg` | [Jcowork_0.2.6_aarch64.dmg](https://github.com/jcowork/jcowork/releases/download/v0.2.6/Jcowork_0.2.6_aarch64.dmg) |

**macOS 安装：**
1. 下载并打开 `.dmg` 文件
2. 将 `Jcowork.app` 拖入「应用程序」文件夹
3. 从「应用程序」或 Spotlight 启动 Jcowork
4. 应用会自动启动后端服务并打开对话窗口

> **注意：** 如果 macOS 提示"Jcowork 已损坏，无法打开"，在终端运行以下命令移除隔离标记：
> ```bash
> xattr -cr /Applications/Jcowork.app
> ```
> 这是因为应用尚未经过 Apple 公证。运行命令后即可正常打开。

**Windows 安装：**
1. 下载 `.msi` 或 `.exe` 安装程序
2. 运行安装程序并按提示完成安装
3. 从开始菜单启动 Jcowork

**从源码构建：**

```bash
# 前置要求：Rust 1.85+、Node.js 20+
cargo install tauri-cli --version "^2"

# 1. 构建前端（必须 — Tauri 将 web/dist/ 打包进应用）
cd web
npm install
npm run build    # 输出到 web/dist/
cd ..

# 2. 构建桌面应用（在项目根目录执行）
cargo tauri build

# 输出：
#   target/release/bundle/dmg/Jcowork_0.2.6_aarch64.dmg   (macOS)
#   target/release/bundle/msi/Jcowork_0.2.6_x64.msi        (Windows)
```

> **重要：** 执行 `cargo tauri build` 前务必先在 `web/` 目录下运行 `npm run build`。Tauri 会将 `web/dist/` 复制到应用包中 — 如果 dist 目录过期或缺失，桌面应用将显示白屏。

> **注意：** 桌面应用需要在 `.env` 中配置至少一个 LLM API Key。Docling PDF 解析服务为可选，如可用会自动连接。

**桌面应用架构：**

桌面应用将完整的 Axum 后端嵌入到 Tauri 前端同一个二进制文件中。启动流程：
1. Axum 服务器在 `127.0.0.1:3000` 启动（带 TCP 就绪检查）
2. WebView 通过 Tauri 的 `custom-protocol`（`tauri://localhost`）加载前端
3. 前端自动检测 Tauri 环境，将所有 API/WebSocket 请求路由到 `http://localhost:3000`

无需单独启动服务进程 — 所有功能集成在单一应用包中。

## 快速开始

### 前置要求

- **Rust** 1.85+（edition 2024）— `rustup` 会通过 `rust-toolchain.toml` 自动安装
- **Node.js** 20+（前端开发）
- **Python** 3.12+（网页搜索和 PDF 解析工具）
- **SQLite** 3.35+（需 FTS5 支持，通常已内置）
- 至少一个 **LLM API Key**（DeepSeek、Qwen、Moonshot、OpenRouter 等）

<details>
<summary>Ubuntu 24.04 一键安装系统依赖</summary>

```bash
sudo apt-get update && sudo apt-get install -y build-essential pkg-config libssl-dev python3 python3-venv nodejs npm
```

- `build-essential` + `pkg-config` + `libssl-dev` — Rust 编译所需（openssl-sys）
- `python3` + `python3-venv` — Python 运行时和虚拟环境支持
- `nodejs` + `npm` — 前端构建

</details>

<details>
<summary>Windows 11 安装说明</summary>

- 安装 [Python 3.12+](https://www.python.org/downloads/)（勾选 "Add to PATH"）
- 安装 [Node.js 18+](https://nodejs.org/)（包含 npm）
- 安装 [Rust（via rustup）](https://www.rust-lang.org/tools/install) — Windows 安装程序自带 MSVC 构建工具
- 或手动安装 Visual Studio Build Tools：`winget install Microsoft.VisualStudio.2022.BuildTools --override '--add Microsoft.VisualStudio.Workload.VCTools --quiet'`

</details>

### 1. 安装 Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env
# rust-toolchain.toml 会在构建时自动选择 Rust 1.85
```

### 2. 配置环境变量

```bash
cp .env.example .env
# 编辑 .env，填入你的 API Key：
#   DEEPSEEK_API_KEY=sk-your-key
#   QWEN_API_KEY=sk-your-key
#   MOONSHOT_API_KEY=sk-your-key
#   # 或 OPENROUTER_API_KEY=sk-your-key
# 设置默认模型：
#   JCWORK_DEFAULT_MODEL=moonshot:kimi-k2.6
```

### 3. 配置 Python 环境（用于网页搜索和 PDF 解析）

**Linux / macOS：**
```bash
make setup-python
```

**Windows (PowerShell)：**
```powershell
powershell -ExecutionPolicy Bypass -File scripts\setup-python.ps1
```

此命令会创建 Python 虚拟环境，包含：
- **playwright** — 无头浏览器，用于网页搜索（搜狗 WAP + Bing 备用）
- **pdftext** — 离线 PDF 文本提取，用于报告解析

### 4. 启动 Docling 服务（用于文档解析与语义搜索）

Docling 服务是 PDF 文档解析和语义搜索的必需组件。直接使用 Python 运行：

```bash
# 激活 Python 虚拟环境（由 make setup-python 创建）
source ~/.jcowork/venv/bin/activate  # Linux/macOS
# 或
.\.jcowork\venv\Scripts\activate     # Windows

# 启动 Docling 服务
cd services/docling
python app.py
```

服务将启动在 `http://localhost:50060`，你应该看到：
```
Loading Docling converter...
Docling converter loaded.
Loading embedding model: paraphrase-multilingual-MiniLM-L12-v2
Embedding model loaded.
INFO:     Uvicorn running on http://0.0.0.0:50060
```

#### 验证 Docling 服务

```bash
# 健康检查
curl http://localhost:50060/health

# 预期响应：
# {"status":"ok","docling_loaded":true,"embedding_loaded":true,"embedding_model":"paraphrase-multilingual-MiniLM-L12-v2","embedding_dim":384}
```

#### 配置

| 环境变量 | 默认值 | 说明 |
|----------|--------|------|
| `DOCLING_SERVICE_URL` | `http://localhost:50060` | Docling 服务端点 |
| `EMBEDDING_DIM` | `384` | 嵌入向量维度 |
| `EMBEDDING_MODEL` | `paraphrase-multilingual-MiniLM-L12-v2` | Sentence transformers 模型 |

> **注意：** Docling 服务首次运行时会下载嵌入模型（约 80MB）。后续启动更快，因为模型已缓存。

### 5. 构建与运行（开发模式）

```bash
# 构建所有 crate
make build

# 初始化数据目录
make setup

# 启动服务器
make run
# 服务器运行在 http://localhost:3000
```

### 6. 前端（开发模式）

前端是基于 React + Vite + TypeScript 的 SPA，位于 `web/` 目录。

```bash
cd web
npm install
npm run dev
# 开发服务器运行在 http://localhost:5173
# API 请求自动代理到后端 :3000
```

**可用脚本：**

| 命令 | 说明 |
|------|------|
| `npm run dev` | 启动 Vite 开发服务器，支持热更新（端口 5173） |
| `npm run build` | 生产构建 → 输出到 `web/dist/` |
| `npm run preview` | 本地预览生产构建结果 |
| `npm run lint` | 运行 ESLint 检查源代码 |

> **开发流程：** 开发时需同时运行 `make run`（后端 :3000）和 `npm run dev`（前端 :5173）。Vite 代理会将 `/api/*` 和 `/api/ws` 转发到后端。
>
> 构建桌面应用前，必须先运行 `npm run build` — Tauri 会将 `web/dist/` 打包进最终产品。

### 7. 验证

```bash
# 健康检查
curl http://localhost:3000/api/health

# 查看已注册的提供者
curl http://localhost:3000/api/providers

# 注册用户
curl -X POST http://localhost:3000/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{"username":"testuser","password":"test123"}'

# 登录（返回 JWT token）
curl -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"testuser","password":"test123"}'
```

## 测试

### 单元测试

```bash
# 运行所有 crate 的单元测试
make test

# 或直接使用 cargo：
cargo test --workspace

# 运行特定 crate 的测试
cargo test -p jcowork-storage
cargo test -p jcowork-memory
cargo test -p jcowork-gateway
```

### 各 Crate 测试内容

| Crate | 测试范围 |
|-------|---------|
| `jcowork-storage` | 数据库创建、每用户连接池、文件操作、路径穿越防护 |
| `jcowork-memory` | 记忆保存/召回/搜索、FTS5 全文检索 |
| `jcowork-skills` | 技能 CRUD、patch 版本控制、frontmatter 解析 |
| `jcowork-gateway` | JWT token 创建/验证、Argon2 密码哈希/验证 |
| `jcowork-agent` | PromptBuilder 组装、注入扫描 |
| `jcowork-tools` | ToolRegistry 分发、schema 生成 |

### 集成测试

```bash
# 用 DeepSeek 测试 key 启动服务器
DEEPSEEK_API_KEY=sk-test JCWORK_PORT=3001 cargo run --bin jcowork &

# 测试认证流程
TOKEN=$(curl -s -X POST http://localhost:3001/api/auth/register \
  -H "Content-Type: application/json" \
  -d '{"username":"integration","password":"test123"}' | jq -r '.token')

echo "获取到 token: $TOKEN"

# 测试聊天接口
curl -X POST http://localhost:3001/api/chat \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"message":"你好，你是谁？"}'

# 停止服务器
kill %1
```

### WebSocket 测试

```bash
# 使用 wscat
npm install -g wscat
wscat -c ws://localhost:3000/api/ws/YOUR_USER_ID

# 发送消息
> {"content":"你好 Agent！"}
```

### 代码检查与格式化

```bash
# 检查格式
make fmt

# 自动修复格式
make fmt-fix

# 运行 clippy 代码检查
make clippy

# 类型检查（不编译）
make check
```

## 部署

### 部署流程

```
1. 编辑 providers.json  → 添加/修改提供者和模型
2. 编辑 .env           → 设置需要启用的提供者的 API Key
3. 构建后端             → cargo build --release （或 make build）
4. 构建前端             → cd web && npm install && npm run build
5. 部署                → 复制二进制、providers.json、.env 和 web/dist/
6. 重启                → 重启服务以加载新配置
```

### 修改配置

**添加新的 LLM 提供者或模型**（使用外部 providers.json 时无需重新编译）：

1. 编辑 `providers.json` — 添加或修改提供者条目
2. 在 `.env` 中添加 API Key（如 `NEWPROVIDER_API_KEY=sk-xxx`）
3. 重启服务器：`kill $(pgrep -f 'target/release/jcowork') && ./target/release/jcowork &`
4. 验证：`curl http://localhost:3000/api/providers`

**修改 API Key 或默认模型**：

1. 编辑 `.env`
2. 重启服务器
3. 无需重新构建

**修改 Rust 源代码**：

1. 编辑源代码文件
2. 重新构建：`cargo build --release`
3. 重启服务器

### providers.json 文件搜索顺序

服务器按以下顺序搜索 `providers.json`：

1. `./providers.json` — 当前工作目录
2. `./config/providers.json` — 配置子目录
3. `/etc/jcowork/providers.json` — 系统配置（Linux）

文件必须存在于以上某个位置；如果找不到 providers.json，服务器将启动失败。

### 方式一：原生二进制

```bash
# 构建后端（release）
cargo build --release

# 构建前端（生产环境）
cd web
npm install
npm run build    # 输出目录：web/dist/
cd ..

# 部署：复制以下文件到服务器
scp target/release/jcowork user@server:/opt/jcowork/
scp providers.json user@server:/opt/jcowork/
scp .env user@server:/opt/jcowork/.env
scp -r web/dist user@server:/opt/jcowork/web/dist
```

在服务器上，需要分别启动**后端**和**Web前端**：

#### 第1步：启动后端

```bash
cd /opt/jcowork
./jcowork
# 后端监听 http://0.0.0.0:3000
```

#### 第2步：使用 nginx 提供 Web 前端服务

安装 nginx（如未安装）：

```bash
# Ubuntu/Debian
sudo apt install nginx

# CentOS/RHEL
sudo yum install nginx
```

创建 nginx 配置文件：

```bash
sudo nano /etc/nginx/conf.d/jcowork.conf
```

粘贴以下配置（完整配置见下方[反向代理](#反向代理nginx)）：

```nginx
server {
    listen 80;
    server_name your-domain.com;  # 改为你的域名或 IP

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

启用并启动 nginx：

```bash
# 测试配置
sudo nginx -t

# 启动 nginx
sudo systemctl enable nginx
sudo systemctl start nginx

# 修改配置后重新加载：
sudo systemctl reload nginx
```

现在可以在浏览器中访问 `http://your-domain.com`。

#### 前端开发模式 vs 生产模式

| 模式 | 命令 | 访问地址 | 启动方式 |
|------|------|---------|----------|
| 开发 | `cd web && npm run dev` | `http://localhost:5173` | Vite 开发服务器（热更新，自动代理 API） |
| 生产 | `npm run build` + nginx | `http://your-domain/` | nginx 提供静态文件 + 代理 API |

开发模式下，只需在 `web/` 目录下运行 `npm run dev` —— Vite 自动处理热更新并将 `/api` 代理到后端 3000 端口。
生产模式下，构建后的静态文件（`web/dist/`）由 nginx 提供服务，同时 nginx 将 `/api` 请求代理到后端。

### 方式二：systemd 服务（Linux）

创建 `/etc/systemd/system/jcowork.service`：
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

# 修改配置后重启：
sudo systemctl restart jcowork

# 查看日志：
journalctl -u jcowork -f
```

### 反向代理（nginx）

nginx 配置负责提供前端静态文件，并将 API/WebSocket 请求代理到后端：

```nginx
server {
    listen 80;
    server_name jcowork.example.com;

    # 前端静态文件
    root /opt/jcowork/web/dist;
    index index.html;

    # API 和 WebSocket 代理 → 后端
    location /api/ {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_read_timeout 86400;  # WebSocket 长连接
    }

    # SPA 回退：其他路由 → index.html
    location / {
        try_files $uri $uri/ /index.html;
    }
}
```

## 配置参考

所有配置通过环境变量设置：

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `JCWORK_HOST` | `0.0.0.0` | 服务器绑定地址 |
| `JCWORK_PORT` | `3000` | 服务器绑定端口 |
| `JCWORK_DATA_DIR` | `~/.jcowork/data` | 数据目录（每用户数据库和工作空间） |
| `JCWORK_JWT_SECRET` | `change-me-in-production` | JWT 签名密钥（生产环境务必修改！） |
| `JCWORK_TOKEN_DURATION_HOURS` | `24` | JWT token 有效期（小时） |
| `JCWORK_DEFAULT_MODEL` | `moonshot:kimi-k2.6` | 默认 LLM 模型（`provider:model` 格式） |
| `DEEPSEEK_API_KEY` | （空） | DeepSeek API Key |
| `QWEN_API_KEY` | （空） | 通义千问 API Key |
| `MOONSHOT_API_KEY` | （空） | Moonshot / Kimi K2.x API Key |
| `OPENROUTER_API_KEY` | （空） | OpenRouter API Key |
| `<PROVIDER>_BASE_URL` | （预设） | 覆盖任意 Provider 的基础 URL |
| `JCWORK_IDLE_TIMEOUT` | `300` | UserActor 空闲超时时间（秒） |

## 支持的 LLM 提供者

所有提供者均使用 OpenAI 兼容的 Chat Completion API，共享同一套流式传输基础设施。
提供者和模型定义从 `providers.json` 加载（见 [providers.json 搜索顺序](#providersjson-文件搜索顺序)）。

| 提供者 | 环境变量 | 默认模型 | 上下文长度 | API 地址 |
|--------|---------|---------|-----------|----------|
| **DeepSeek** | `DEEPSEEK_API_KEY` | `deepseek-v4-flash` | 64K | `api.deepseek.com` |
| **Qwen** | `QWEN_API_KEY` | `qwen3.6-plus` | 131K | `dashscope.aliyuncs.com/compatible-mode/v1` |
| **Moonshot** | `MOONSHOT_API_KEY` | `kimi-k2.5` | 256K | `api.moonshot.cn/v1` |
| OpenRouter | `OPENROUTER_API_KEY` | `anthropic/claude-3.5-sonnet` | 200K | `openrouter.ai/api/v1` |
| 本地/Ollama | （无需 key） | `qwen3.5:35b-a3b` | 32K | `localhost:11434/v1` |

### 模型选择

使用 `provider:model` 格式选择模型：

```
deepseek:deepseek-v4-flash   # DeepSeek V4 Flash
qwen:qwen3.6-plus            # 通义千问 Qwen3.6 Plus
moonshot:kimi-k2.6           # Moonshot Kimi K2.6
openrouter:anthropic/claude-3.5-sonnet  # 通过 OpenRouter
local:qwen3.5:35b-a3b        # 本地 Ollama
```

提供者在 API Key 环境变量非空时自动注册。服务器启动时会打印已注册的提供者列表。

### 添加自定义提供者

添加新提供者只需编辑 `providers.json`：

```json
{
  "id": "my-provider",
  "name": "我的自定义提供者",
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

然后在 `.env` 中添加 API Key 并重启服务器即可，无需重新编译。

## 飞书（Lark）机器人接入

Jcowork 支持飞书作为 Web UI 之外的输入渠道。每个用户可以配置自己的飞书机器人，让飞书上的联系人与自己的 AI Agent 对话。

### 工作原理

1. 每个 jcowork 用户在 **设置** 页面配置自己的飞书自建应用
2. 当有人在飞书上给机器人发消息时，事件通过 Webhook 传递到 Jcowork
3. Jcowork 通过事件头中的 `app_id` 匹找到对应的 jcowork 用户
4. 消息经过完整的 Agent 循环处理（包含该用户的记忆、技能、工具、提醒）
5. Agent 的回复作为飞书消息返回

### 配置步骤

1. **创建飞书自建应用** — 在[飞书开放平台](https://open.feishu.cn/app)创建应用
   - 启用「机器人」能力
   - 在**权限管理**中，开通 `im:message`（接收消息）和 `im:message:send_as_bot`（发送消息）

2. **配置事件订阅地址**
   - 在应用的**事件订阅**页面，设置请求地址为：
     ```
     https://你的jcowork域名/api/feishu/event
     ```
   - 订阅事件：`im.message.receive_v1`
   - 飞书会发送验证请求来确认 URL 有效性，Jcowork 会使用你配置的 Verification Token 自动完成验证

3. **在 Jcowork 中配置**
   - 登录 Jcowork Web 界面
   - 进入 **Settings** → **Feishu Integration**
   - 填写：
     - **App ID** — 来自飞书开放平台（如 `cli_xxxxx`）
     - **App Secret** — 来自飞书开放平台
     - **Verification Token** — 来自事件订阅页面
     - **Encrypt Key**（可选）— 如果开启了事件加密
   - 点击 **Save**

4. **开始对话** — 在飞书上给机器人发消息，Agent 会使用你配置的模型、记忆和技能来回复

### 按用户隔离架构

- 每个 jcowork 用户配置**自己的**飞书应用，无需共享凭据
- 飞书消息通过 `app_id` 匹找到对应的 jcowork 用户
- 飞书渠道支持所有按用户功能：记忆、技能、提醒、自定义 Agent 身份
- 受技能门控的工具（如 `web_search`）仅在用户启用对应技能后可用

### API 端点

| 方法 | 路径 | 认证 | 说明 |
|------|------|------|------|
| POST | `/api/feishu/event` | 无 | 飞书事件回调（Webhook） |
| GET | `/api/feishu/config` | 需要 | 获取当前用户的飞书配置 |
| PUT | `/api/feishu/config` | 需要 | 保存/更新飞书配置 |
| DELETE | `/api/feishu/config` | 需要 | 删除飞书配置 |

### 通过 Web UI 配置

飞书配置通过 Web 设置页面按用户管理，无需设置环境变量。

## 文档索引与语义搜索

Jcowork 支持自动文档索引和语义搜索。上传 PDF 或 Markdown 文档到工作空间后，系统会自动解析并建立索引，实现智能检索。

### 工作原理

1. **文档上传** — 在文档页面上传 PDF 或 Markdown 文件
2. **自动解析** — PDF 使用 [Docling](https://github.com/DS4SD/docling)（IBM 文档理解库）解析为结构化 Markdown，保留表格和图片
3. **向量索引** — 文档分块后通过本地 sentence-transformers 模型生成嵌入向量，存入 SQLite 用于语义搜索
4. **智能检索** — 提问时系统通过向量相似度搜索检索相关文档片段

### 架构

```
┌─────────────────────────────────────────────────────────┐
│                   文档处理流水线                           │
├─────────────────────────────────────────────────────────┤
│  PDF 上传 → Docling 服务 → Markdown + 表格              │
│                                    ↓                     │
│                    文档分块器（按标题/表格）               │
│                                    ↓                     │
│              嵌入服务（sentence-transformers）            │
│                                    ↓                     │
│              SQLite 向量存储（BLOB + 余弦相似度）         │
└─────────────────────────────────────────────────────────┘
```

### 组件

| 组件 | 说明 |
|------|------|
| **Docling 服务** | Python FastAPI 服务，运行在 50060 端口，处理 PDF→Markdown 转换 |
| **嵌入服务** | Docling 服务的一部分，使用 `paraphrase-multilingual-MiniLM-L12-v2` 生成 384 维向量 |
| **文档分块器** | Rust 模块，按标题和表格将 Markdown 拆分为块 |
| **向量存储** | SQLite 表（`doc_chunks`、`chunk_embeddings`），支持 FTS5 回退 |
| **文档检索工具** | Agent 工具，用于文档分块的语义搜索 |

### 部署

Docling 服务作为 Python FastAPI 服务器运行：

```bash
# 激活 Python 虚拟环境
source ~/.jcowork/venv/bin/activate  # Linux/macOS
.\.jcowork\venv\Scripts\activate     # Windows PowerShell

# 启动服务
cd services/docling
python app.py
```

服务将运行在 `http://localhost:50060`。

### 配置

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `DOCLING_SERVICE_URL` | `http://localhost:50060` | Docling 服务端点 |
| `EMBEDDING_DIM` | `384` | 嵌入向量维度 |
| `EMBEDDING_MODEL` | `paraphrase-multilingual-MiniLM-L12-v2` | Sentence transformers 模型 |

### 使用方式

当你在聊天中附加文档时，系统会自动：
1. 检索文档的完整内容（最多 15,000 字符）
2. 作为参考资料注入 LLM 上下文
3. LLM 可直接基于文档内容回答问题

对于较长文档或更精确的检索，LLM 还可使用 `doc_retrieve` 工具对文档分块进行语义搜索。

## API 参考

### 认证

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/auth/register` | 注册新用户 |
| POST | `/api/auth/login` | 登录，返回 JWT |

### 聊天

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/api/chat` | 发送消息，获取回复 |
| WS | `/api/ws/{user_id}` | WebSocket 流式聊天 |

### 数据

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/api/sessions` | 列出用户会话 |
| GET | `/api/skills` | 列出用户技能 |
| POST | `/api/skills` | 创建技能 |
| GET | `/api/memory` | 列出用户记忆 |
| GET | `/api/health` | 健康检查 |

## Agent 工具

Jcowork Agent 内置了 LLM 在对话中可自动调用的工具，无需特殊语法，自然语言即可触发。

### 提醒（一次性）

在指定时间设置一次性提醒，到点后自动推送通知。

| 工具 | 说明 |
|------|------|
| `reminder_add` | 设置一次性提醒 |
| `reminder_list` | 列出所有未触发的提醒 |
| `reminder_remove` | 按 ID 取消提醒 |

**示例：**

```
你: 提醒我下午3点开会
Agent: 🔔 提醒已设置！下午3:00我会提醒你：开会

[下午3点，你收到推送通知：]
🔔 Reminder: 开会
```

### 定时任务（周期性 Cron）

使用标准 cron 表达式（5字段：分 时 日 月 周）设置周期性定时任务。

| 工具 | 说明 |
|------|------|
| `cron_add` | 创建周期性定时任务 |
| `cron_list` | 列出所有定时任务 |
| `cron_remove` | 按 ID 删除定时任务 |

**Cron 表达式示例：**

| 表达式 | 含义 |
|--------|------|
| `0 9 * * *` | 每天早上9点 |
| `0 9 * * 1-5` | 工作日早上9点 |
| `0 9 * * 1` | 每周一早上9点 |
| `*/30 * * * *` | 每30分钟 |
| `0 8,18 * * *` | 每天早8点和晚6点 |
| `0 9 1 * *` | 每月1号早上9点 |

**示例：**

```
你: 每天早上9点提醒我写日报
Agent: ✅ 定时任务已创建！计划：0 9 * * * — 每天早上9:00提醒你写日报
```

> **注意：** 提醒和定时任务目前存储在内存中，服务重启后会丢失。持久化存储将在后续版本中支持。

## 核心设计决策

1. **每用户独立 SQLite** — 每个用户拥有独立的数据库文件，使用 WAL 模式支持并发读取，杜绝跨用户数据泄露。

2. **Actor 模型** — 通过 tokio 任务 + mpsc 通道实现 UserActor 模式。无跨用户共享可变状态，零锁竞争，隔离干净。

3. **Provider 架构** — 记忆、LLM、上下文引擎均采用 trait-based provider 模式，可替换实现而无需修改核心逻辑。

4. **SSE 流式传输** — LLM 响应通过 SSE 经 HTTP 或 WebSocket 流式返回，工具调用与文本增量交替输出。

5. **技能自我改进** — Agent 从经验中创建技能并在使用中修补。

6. **记忆提示** — 定期提醒 Agent 将知识持久化为声明性事实。

7. **上下文压缩** — 当接近 token 上限时，压缩旧消息同时保护系统提示和最近上下文。

8. **静态二进制** — 单一二进制文件，无运行时依赖。

## 技术栈

| 层级 | 技术 |
|------|------|
| 语言 | Rust (edition 2024) |
| 异步运行时 | tokio |
| Web 框架 | axum |
| 数据库 | SQLite (sqlx, WAL 模式, FTS5) |
| 认证 | JWT (jsonwebtoken) + Argon2 |
| LLM 客户端 | reqwest + SSE 流式（5 个提供者） |
| 并发 | DashMap, mpsc channels |
| 前端 | React + Vite + TypeScript |
| 桌面应用 | Tauri v2（原生 macOS/Windows） |

## 许可证

Apache-2.0
