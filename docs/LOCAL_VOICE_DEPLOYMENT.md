# 全双工语音本地化部署方案（开源模型替代 DashScope 云端 ASR/TTS）

> 状态：**方案 B 已实施**（本地 sherpa-onnx sidecar + 云端/本地开关 + 云端不可用自动回退）；方案 A（GPU 档）仍为规划。
> 目标：用可本地部署的开源模型替换当前云端 `qwen3.8-omni-flash-realtime`（流式 ASR）与
> `qwen-audio-3.1-tts-plus`（流式 TTS），**保留现有全双工体验**：server-VAD 打断（barge-in）、
> 流式转写气泡、双路输出（聊天详情 + 简洁口语播报）、`voice_stage` 落盘归因。
> 范围：只替换「语音 ↔ 文本」两端；中间的 agent 工具循环、播报改写轮（spoken_summary）不动。
>
> **已落地（方案 B）**：
> - 后端开关：Settings 语音卡片「语音后端：云端 / 本地」（`VoiceConfig.backend`，热切换）；
> - 自动回退：选云端时，ASR/TTS 连接失败或会话中断 → 自动改连本地 sidecar，落盘 `backend_fallback`；
> - sidecar：`services/voice-lite/app.py`（sherpa-onnx 2.0.3，纯 CPU，端口 50061，/asr + /tts + /health）；
> - 安装/启动：`make setup-voice-lite` / `make voice-lite`（或 scripts/setup-voice-lite.sh / start-voice-lite.sh）；
> - Rust 侧：`jcowork-voice::{LocalAsrClient, LocalTtsClient, AsrSession, TtsSession}`，gateway 编排零改动复用。

---

## 1. 现状与替换点

当前链路（详见 `voice_flow_analysis.tmp.md`）：

```
浏览器麦克风 PCM 16k ──WS──▶ gateway /api/voice
   ├─ M3 OmniAsrClient ──wss──▶ DashScope Omni realtime（云端 ASR，server_vad）
   ├─ M4-M7 agent_loop（工具循环，云端/任意 LLM）──▶ 聊天详情
   ├─ M11 spoken_summary 播报改写（同 LLM）
   └─ M8-M9 SpeechSegmenter → TtsClient ──wss──▶ DashScope 原生双工 TTS（云端）
                                        ◀──binary PCM 24k── tts_audio → 浏览器
```

需要替换的只有两个 WS 客户端，且它们的事件接口已经是稳定契约：

| 模块 | 文件 | 事件契约（保持不变） |
|---|---|---|
| ASR 客户端 | `crates/jcowork-voice/src/omni_asr.rs` | `AsrEvent::{Ready, SpeechStarted, SpeechStopped, TranscriptDelta{text,stash}, TranscriptCompleted{transcript}, Error, Closed}` |
| TTS 客户端 | `crates/jcowork-voice/src/tts.rs` | `TtsEvent::{AudioDelta(Vec<u8> /*24k s16le mono*/), ResponseDone, Error, Closed}` |
| 编排 | `crates/jcowork-gateway/src/voice.rs` | barge-in / 双路输出 / 分句 / 日志，全部复用 |
| 配置 | `crates/jcowork-voice/src/config.rs` | `VoiceConfig`（app_settings key `voice`，Settings 页管理，热切换） |

**结论：gateway 编排层零改动或近零改动；新增两个本地客户端 + 配置后端开关即可。**

---

## 2. 开源模型选型

### 2.1 流式 ASR（替代 Omni realtime 的转写 + server_vad 部分）

| 候选 | 流式 | 中文效果 | 资源 | 许可 | 评价 |
|---|---|---|---|---|---|
| **FunASR Paraformer-zh-streaming**（+FSMN-VAD +CT-Transformer 标点） | ✅ 真流式（chunk 约 600ms 可调） | 中文一线水平，带标点/ITN | CPU 可跑（RTF<0.5），GPU 更佳；官方提供 runtime SDK 与 WebSocket 服务 | 代码 MIT；模型 ModelScope 许可（可商用，发布前核对模型卡） | **主推（方案 A）**：中文、流式、VAD、标点一条龙，与现有「partial 前缀+后缀」语义最接近 |
| **sherpa-onnx streaming paraformer（zh-en 双语，FunASR 导出）** | ✅ 真流式 | 中文一线水平（实测整句零错字） | 纯 CPU、RTF≈0.03，fp32 内存 ~1GB | Apache-2.0 | **主推（方案 B，Mac/低配机）**：中文准确率显著优于 zipformer；int8 与 fp32 精度接近，但为稳妥用 fp32 |
| sherpa-onnx streaming zipformer（zh-en 双语，2023-02-20） | ✅ 真流式（chunk 320ms 级） | 一般：同音错字多；**int8 量化会出现重复字（"大大大模型型"）** | 纯 CPU、内存 <1GB | Apache-2.0 | 降级备选：仅当 paraformer 模型缺失时启用，且必须 fp32（int8 重复字不可接受） |
| SenseVoice-Small + silero-VAD 切段 | ⚠️ 伪流式（VAD 切段后整段识别） | 很好，多语种 | CPU 快 | 模型许可需核对 | 备选：实现简单，但 partial 体验差（无逐字上屏），只在整句结束时出文本 |
| faster-whisper / whisper.cpp | ❌ 滑窗伪流式 | 中文一般、易幻觉 | GPU 才实用 | MIT | 不推荐：延迟与幻觉都不适合对话场景 |

VAD 统一说明：云端 Omni 的 `server_vad`（SpeechStarted 触发 barge-in）在本地由
**FSMN-VAD（方案 A）或 silero-VAD（方案 B）** 承担，在 sidecar 内做，网关侧协议不变。

### 2.2 流式 TTS（替代 Qwen-Audio-TTS-Plus）

| 候选 | 流式首包 | 音质/自然度 | 资源 | 许可 | 评价 |
|---|---|---|---|---|---|
| **CosyVoice2-0.5B**（FunAudioLLM） | 官方标称 ~150ms（GPU，收 5 字即合成） | 中文对话级第一梯队，支持零样本音色克隆、语速控制 | GPU 建议 ≥6GB 显存；CPU 可跑但首包秒级 | Apache-2.0（代码+权重） | **主推（方案 A）**：与云端体验最接近，克隆 3–10s 参考音频即可复刻 `longanlingxin` 类音色 |
| **sherpa-onnx vits-melo-tts-zh_en / matcha-icefall-zh-baker** | 纯 CPU 即近实时（整句合成后分块下发） | 清晰自然，但「播音腔」，韵律逊于 CosyVoice2 | 纯 CPU，内存 <1GB，Mac/Win/Linux | Apache-2.0 / MIT | **主推（方案 B）**：零 GPU 依赖，句子级流式（配合现有 SpeechSegmenter 天然契合） |
| Fish-Speech / OpenAudio S1 | 流式支持 | 很好，克隆强 | GPU | ⚠️ 权重非商用许可（CC-BY-NC-SA 系），商用需授权 | 备选（仅内部/非商用） |
| GPT-SoVITS v2/v3 | 流式支持，首包 ~500ms | 少样本克隆音色最像 | GPU，微调门槛高 | MIT | 备选：需要固定专属音色且愿意微调时 |
| IndexTTS2 | 支持 | 很好，情感可控 | GPU | 核对模型卡 | 备选 |
| ChatTTS | 流式 | 对话感强但稳定性一般 | GPU 小 | ⚠️ 权重非商用 | 不推荐主用 |

### 2.3 端到端全双工 Omni 模型（为什么不作为主路线）

MiniCPM-o 4.5（9B，llama.cpp 量化后 12GB 显存 RTX 5070 全双工 RTF≈0.4）、GLM-4-Voice、
Qwen2.5-Omni-7B 等开源全模态模型可以「边听边说」，但它们**自带 LLM，会绕过 jcowork 的
agent 工具循环、技能、记忆与播报改写**——这正是本产品的核心价值。因此：

- 主路线保持 **ASR → agent_loop → 改写 → TTS** 的分离式管线，只换两端模型；
- Omni 模型仅作为未来「纯闲聊模式」的可选项，不在本方案范围。

---

## 3. 部署拓扑（两档，可按机器自动降级）

### 方案 A：有 GPU 的 Linux 服务器（体验对齐云端）

```
jcowork gateway (Rust)
  ├─ WS ws://127.0.0.1:50061/asr   → voice-asr sidecar（FunASR runtime：
  │                                   paraformer-zh-streaming + fsmn-vad + 标点）
  └─ WS ws://127.0.0.1:50062/tts   → voice-tts sidecar（CosyVoice2-0.5B，
                                      FastAPI/WS 包装，流式 yield PCM 24k）
```

- 两个 sidecar 沿用 `services/docling` 的既有模式：`services/voice-asr/`、`services/voice-tts/`
  各含 `app.py + requirements.txt（锁版本）+ Dockerfile`，配套 `scripts/setup-voice.sh/.ps1`。
- 模型权重从 ModelScope 下载（国内快），缓存到 `JCWORK_DATA_DIR/models/`，首次启动自动拉取。
- docker compose 增加两个服务（GPU 直通 `deploy.resources.reservations.devices`），
  与现有 docling 服务并列；无 GPU 时 compose profile 切方案 B。

### 方案 B：纯 CPU / macOS（MacBook Air 等端侧，零 GPU）

```
jcowork gateway (Rust)
  └─ WS ws://127.0.0.1:50061/voice → voice-lite sidecar（单进程 sherpa-onnx：
        streaming paraformer zh-en ASR（fp32，zipformer fp32 备选）+ silero-VAD + vits-melo-tts-zh_en TTS）
```

- 单个 Python sidecar（`sherpa-onnx` pip 包，ONNX Runtime CPU），同一端口两条 WS 路由；
  内存合计约 1–1.5GB，Apple Silicon 实测可实时。
- TTS 为句子级合成：现有 `SpeechSegmenter` 已按句喂入，sidecar 合成一句立即分块回传，
  听感上仍是连续播报，只是首包比方案 A 慢。
- 桌面 Tauri 场景：sidecar 随内嵌服务器由 setup 脚本安装到用户 venv（同 docling/python 模式）。

### 延迟预算（对照云端实测）

| 阶段 | 云端实测（loopback） | 方案 A 目标 | 方案 B 目标 |
|---|---|---|---|
| ASR 就绪（asr_ready） | 296ms | <500ms（本地连接） | <500ms |
| partial 上屏粒度 | ~300–600ms | chunk 600ms 可调至 320ms | ~320ms |
| 语音结束→asr_final | ~700ms（VAD 尾判） | VAD 尾判 500–800ms（可调） | 同左 |
| TTS 首包（ttfa） | 572–716ms | **<300ms**（CosyVoice2 GPU） | <1.5s（首句 CPU 合成） |
| 合成速度 | 3.6× 实时 | ≥3× 实时 | ≥1.5× 实时 |

> 双路输出下「开口延迟」主要由改写轮决定（详情流结束 + ~1s 改写 + ttfa），
> 本地化后 ttfa 项在方案 A 反而优于云端；方案 B 首句略慢但后续句无缝衔接。

---

## 4. Sidecar WS 协议（对齐现有事件契约）

### 4.1 ASR `/asr`（16k s16le mono 二进制帧上行，与浏览器→网关格式一致）

上行：binary PCM 帧（网关原样转发，无需重采样）；可选 JSON `{"type":"config","chunk_ms":600,"vad_tail_ms":700}`。

下行 JSON → `AsrEvent` 映射：

| sidecar 消息 | AsrEvent |
|---|---|
| `{"type":"ready"}` | `Ready` |
| `{"type":"speech_started"}` | `SpeechStarted`（barge-in 触发，语义=Omni server_vad） |
| `{"type":"speech_stopped"}` | `SpeechStopped` |
| `{"type":"partial","text":"确认前缀","stash":"暂定后缀"}` | `TranscriptDelta{text,stash}` |
| `{"type":"final","text":"整句"}` | `TranscriptCompleted{transcript}` |
| `{"type":"error","message":…}` / WS close | `Error` / `Closed`（close code 1000 视为良性，沿用 tts.rs 现有约定） |

FunASR 的 online 结果天然有「已确认前缀 + 当前 chunk 暂定」两态，映射 `text/stash` 无损；
sherpa-onnx 用 endpoint 规则产出 final，partial 取当前解码结果。

### 4.2 TTS `/tts`（JSON 文本下行流式喂入，binary PCM 24k 上行）

| 方向 | 消息 | 对应现有 TtsClient 动作 |
|---|---|---|
| 上行 | `{"type":"start","voice":"…","speed":1.0}` | run-task（建会话/选音色） |
| 上行 | `{"type":"text","text":"句子"}`（可多条） | continue-task / append_text |
| 上行 | `{"type":"finish"}` | finish-task |
| 下行 | binary 帧 = PCM 24k s16le mono | `AudioDelta` |
| 下行 | `{"type":"done"}` | `ResponseDone` |
| 下行 | `{"type":"error","message":…}` | `Error` |

- 采样率统一 24k（与浏览器 AudioContext(24000) 一致）；sherpa-onnx vits 原生 22.05k/16k，
  sidecar 内重采样到 24k 后下发，网关与前端零改动。
- 打断 = 网关直接 drop 连接（现有语义），sidecar 检测到断连立即停止推理释放资源。

---

## 5. jcowork 代码改动清单

| # | 位置 | 改动 |
|---|---|---|
| 1 | `jcowork-voice/src/config.rs` | `VoiceConfig` 增加 `backend: "cloud" \| "local"`（默认 cloud）、`local_asr_url`、`local_tts_url`、`local_voice`；`is_configured()` 按 backend 分支（local 不需要 api_key/workspace）；env：`JCWORK_VOICE_BACKEND`、`JCWORK_VOICE_LOCAL_ASR_URL`、`JCWORK_VOICE_LOCAL_TTS_URL` |
| 2 | `jcowork-voice/src/local_asr.rs`（新） | `LocalAsrClient`：连 sidecar `/asr`，转发 PCM，按 §4.1 映射为 **同一个 `AsrEvent`** |
| 3 | `jcowork-voice/src/local_tts.rs`（新） | `LocalTtsClient`：按 §4.2 实现 `append_text/finish`，产出 **同一个 `TtsEvent`** |
| 4 | `jcowork-voice/src/lib.rs` | 导出新客户端；可提供 `enum AsrBackend/TtsBackend` 统一句柄，或在 gateway 工厂处分支 |
| 5 | `jcowork-gateway/src/voice.rs` | 仅两处工厂分支：会话建立时按 `cfg.backend` 选 Omni/Local ASR；`ensure_tts` 内选云端/Local TTS。**双路输出、barge-in、voice_stage 日志全部原样复用**（stage 名不变，`data` 里加 `backend` 字段便于归因对比） |
| 6 | `GET/PUT /api/admin/voice` + Settings 语音卡片 | 增加「后端：云端 / 本地」切换与本地服务地址、音色字段；local 模式下隐藏 DashScope key/workspace 必填校验；`GET /api/voice/config` 的 enabled 判定同步分支 |
| 7 | `services/voice-asr/`、`services/voice-tts/`、`services/voice-lite/`（新） | app.py + 锁版本 requirements + Dockerfile（照抄 docling 模式） |
| 8 | `scripts/setup-voice.sh/.ps1`（新） | venv 安装 + 模型下载（ModelScope 优先，HF 镜像兜底）+ 就绪标记，幂等 |
| 9 | `docker-compose` / Makefile | 增加 voice 服务（GPU profile）与 `make voice-lite`（CPU profile） |

改动量估计：Rust 侧 ~600 行（两个新客户端 + 配置/工厂分支 + 测试），Python sidecar ~400 行，
前端 Settings ~100 行。**gateway 编排与前端语音 UI 不动。**

---

## 6. 音色方案（替代 longanlingxin_v3.1）

- 方案 A：CosyVoice2 零样本克隆——录 3–10s 参考音频（或选用开源参考音色），
  `local_voice` 存参考音频路径/预置音色名；支持语速参数。可在 Settings 上传参考音频。
- 方案 B：vits-melo-tts-zh_en 自带说话人 id（`local_voice` = sid 数字），
  matcha-icefall-zh-baker 为单说话人女声；无需克隆。
- 许可注意：克隆他人音色需取得授权；参考音频仅存本机 `JCWORK_DATA_DIR`，不出内网。

---

## 7. 风险与回退

| 风险 | 缓解 |
|---|---|
| CPU 机器 TTS 首包慢（方案 B） | 句子级流式掩盖；Settings 可一键切回 cloud（配置热切换已支持，无需重启） |
| 中文识别率不及云端 Omni | Paraformer 中文基准接近；保留 cloud 后端做 A/B；`voice_stage` 加 backend 字段常态化对比 asr_final 耗时与人工抽检 |
| 显存/内存不足 | 方案 A 需 ≥6GB 显存（TTS）+ ~2GB（ASR）；不足自动降级方案 B；sidecar 健康检查失败时 Settings 显示不可用原因 |
| 模型许可 | 主路线全 Apache/MIT/ModelScope 可商用；Fish-Speech/ChatTTS 权重非商用，仅列备选不入默认 |
| sidecar 崩溃 | 网关现有 Error/Closed → toast 路径复用；setup 脚本 + 健康端点 `/health`；WS close 1000 良性语义沿用 |
| 依赖体积 | sherpa-onnx 方案 ~几百 MB；CosyVoice2 权重 ~1–2GB；均缓存数据目录，不入 git |

**回退策略：`backend` 是配置项而非编译开关，任何时刻可在 Settings 切回云端，零代码回滚。**

---

## 8. 实施顺序与验收

1. **M1**：config/backend 开关 + `LocalAsrClient`/`LocalTtsClient` + 单测（mock WS server，
   覆盖 partial/final/barge-in/断连、PCM 透传、finish 时序）。
2. **M2**：`services/voice-lite`（sherpa-onnx 单进程）+ setup 脚本 → Mac 本机跑通。
3. **M3**：`services/voice-asr` + `services/voice-tts`（GPU 档）+ compose profile。
4. **M4**：Settings UI + admin API + `voice_stage` backend 字段 + 文档。

验收（复用既有 loopback 回路脚本与 barge-in 断言）：

- [x] 合成语音喂入 → partial 流式上屏 → asr_final 文本与语料一致（方案 B 实测，见 §9）
- [x] 双路输出：聊天详情完整，TTS 播报为改写稿；`spoken_start/spoken_first_text/tts_*` 落盘齐全
- [x] barge-in：播报中打断无残留音频（stale=[]），新一轮独立播报
- [x] 延迟达标：方案 B 实测 ttfa 207–832ms（优于 <1.5s 目标），CPU 合成 3.3× 实时
- [x] Settings 云端↔本地热切换不重启即生效；云端故障自动回退（`backend_fallback` 落盘）
- [x] `cargo test -p jcowork-voice`（19）+ gateway voice 单测（3）+ api_integration（60）全绿

---

## 9. 方案 B 实测记录（2026-11-25，MacBook Air M 系列，纯 CPU）

环境：sherpa-onnx 2.0.3（streaming zipformer bilingual zh-en int8 + silero VAD + vits-melo-tts-zh_en），
sidecar 端口 50061；回路脚本用 sidecar 自身 TTS 合成测试语音（24k→16k 重采样）喂入 /api/voice。

| 阶段 | 结果 | 关键数据（voice_stage 落盘） |
|---|---|---|
| 本地全链路 | PASS | asr_ready 4ms；partial 5 次；asr_final 2328ms；ttfa **207ms**；8.1s 音频合成 2435ms（3.3× 实时） |
| 云端故障自动回退 | PASS | 云端 URL 指向死端口：1ms 内 `backend_fallback{component:asr,source:connect}`，TTS 同样回退；通话不中断 |
| 播报中打断 | PASS | 首包 5.48s 时喂第二句 → 5.86s tts_cancel；宽限窗外无残留音频；turn1 无 tts_done（合成被取消），turn2 独立播报 |

实现中的适配点：sherpa-onnx 2.x 移除了高层 `Vad` 包装，sidecar 用 `VadModel.is_speech` 窗口分类 +
滞回（≥150ms 连续语音触发 speech_started，≥300ms 静音触发 speech_stopped）实现 barge-in；
ASR 尾判用 endpoint 规则（rule2 0.7s）对齐云端 server_vad 手感。

运维：`make setup-voice-lite`（venv + 模型下载，幂等）→ `make voice-lite`（前台运行）；
健康探针 `GET http://127.0.0.1:50061/health` 返回 `{asr, tts}` 就绪状态。桌面端需另行启动该
sidecar（不随 Tauri 进程）；未启动时云端模式不受影响，仅回退/本地模式不可用。

### 9.1 本地识别质量归因与模型更换（2026-11-25 晚）

现象：真实麦克风下本地 ASR 出现重复字与错字（"一下语言严大大大模型型的最新进正阵进展展"），
同一句云端识别完全正确。归因实验（同一音频喂三条路径：无 endpoint 整段解码 / endpoint+reset /
sidecar 实时 WS）输出完全一致 → **排除链路（重复喂入、reset 错位、VAD、前端采集）**；
换 fp32 后重复字消失但仍有同音错字 → **根因是模型：2023-02 版 zipformer 中文能力弱 + int8 量化放大重复字**。

对照（参考句"介绍一下大语言模型的最新进展。"）：

| 模型 | 识别结果 |
|---|---|
| zipformer int8（原配置） | 介绍一下大语炎炎摹模型的最新进展站（重复字） |
| zipformer fp32 | 介绍一下大雨言模型的最新进展（同音错字） |
| **paraformer fp32（现配置）** | **介绍一下大语言模型的最新进展（零错字）** |

更换后 sidecar 实时链路复验：参考句整句正确；真人样本 0/1/2.wav 整句完整出 final（含尾句），
RTF≈0.03（30× 实时），CPU 余量充足。已知取舍：paraformer 导出源自 zh-cn 通用模型，中英混读时
英文弱于 zipformer（小写、偶发错词）；本地模式以中文优先，混读场景建议云端。
模型目录：`sherpa-onnx-streaming-paraformer-bilingual-zh-en`（主）与 zipformer（fp32 备选），
`setup-voice-lite.sh` 两者都下载；sidecar 启动日志 `ASR ready: streaming-paraformer…(fp32)` 可确认生效。

---

## 附：选型速查

| 场景 | ASR | TTS | 硬件 |
|---|---|---|---|
| 有 GPU 的服务器（推荐，体验≈云端） | FunASR paraformer-zh-streaming + FSMN-VAD + 标点 | CosyVoice2-0.5B（流式 + 克隆） | ≥6GB 显存 |
| Mac / 纯 CPU / 内网低配机 | sherpa-onnx streaming paraformer zh-en（fp32）+ silero-VAD | sherpa-onnx vits-melo-tts-zh_en | 内存 ~2GB |
| 未来纯闲聊全双工（不在本期） | MiniCPM-o 4.5 / GLM-4-Voice（端到端，绕过 agent，慎用） | — | ≥12GB 显存 |