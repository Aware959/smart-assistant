# Implementation Plan: Telegram 语音消息识别

**Branch**: `001-telegram-voice-transcription` | **Date**: 2026-10-19 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/001-telegram-voice-transcription/spec.md`

## Summary

让 Telegram 入站的语音消息（语音条与音频文件）先转成文字，再作为该条用户消息的内容进入既有对话流水线——用户发语音，助手像收到文字一样理解并回复（FR-001/002）。

技术路径（选型 C，用户拍板）：

1. **本机媒体网关新增转写端点** `POST /transcribe`，内部实现可插拔 ASR 引擎，默认 `faster-whisper`（懒加载，未安装即明确报"引擎不可用"）。
2. **Rust 侧在通道层解析入站语音**：`getFile` 取 `file_path` → 下载音频字节 → POST 给网关 → 得到转写文本，然后在**落库之前**把文本当作本条用户消息的内容，后续流程（落库、起轮次、记忆沉淀）完全不变。
3. **失败按性质分流**（本特性的核心判断）：音频内容层面的问题（听不清/解码失败/超长）落一条**说话人视角的标记文本**交给模型，由模型自己用自然语言回应（FR-005，不写死话术）；能力层面的问题（未启用/网关不可达/引擎未装）走**系统故障路径**——记错误、不起轮次、不伪装成"没听清"（FR-006/013）。

## Technical Context

**Language/Version**:
- `backend/` — Rust，版本由根目录 `rust-toolchain.toml` 钉死（stable + rustfmt/clippy）。
- `media-gateway/` — Python。整体仍保持**纯标准库**；ASR 引擎为可选依赖，且**开启 ASR 时**要求解释器为 **CPython 3.13，由 uv 安装**（Spike 实测：`py -3.13` 与系统 3.15 均不可用，见 research.md D2）。

**Primary Dependencies**:
- Rust：**不新增任何 crate**。复用现有 `reqwest`（json + multipart）、`serde`、`tracing`、`chrono`。
- Python：标准库（`http.server` / `urllib` / `importlib.util`）为主；ASR 引擎 `faster-whisper`（可选、懒加载，形态对齐既有 `edge-tts`），安装时 MUST 锁 **`av<19`**（`av 19.x` 与 faster-whisper 1.2.1 不兼容，装上即崩——Spike 实测）。

**Storage**: **无 schema 变更**。转写文本直接作为 `messages.content` 落库；"源自语音"这一事实用内容前缀 `[语音] ` 表达（FR-009），因此不需要新增列、不需要迁移、也不需要 `sa-mem check`。

**Testing**:
- Rust：`cargo test`；新增单测放既有独立文件 `backend/src/channels/telegram_tests.rs`（遵循宪法原则 V）。
- Python：`cd media-gateway && python -m unittest discover -v`；新增 `media-gateway/tests/test_asr.py`，用假引擎覆盖分派与错误映射，**不依赖 faster-whisper 真的装上**。

**Target Platform**: Windows 本机。验证走 `cargo run --bin sa-link`（Telegram 长轮询）+ `python main.py`（网关监听 `127.0.0.1:8787`）。

**Project Type**: 单仓库多组件 —— Rust 后端（含 Telegram 通道）+ Python 本机媒体网关。

**Performance Goals**: 一条 30 秒内的语音，从收到到给出回复，总耗时不超过同内容文字消息的 2 倍（SC-001）。Spike 实测 `small` 在 CPU 上 RTF = 0.69x ~ 0.98x，冷启动加载 **21~46s**（靠启动预热消除）。

**Constraints**:
- Telegram Bot API 的 `getFile` 下载上限 **20 MB**；本特性另设**时长前置上限**与体积上限（FR-003，见 research.md D10）。
- 转写发生在**长轮询循环内**（与既有"用户消息落库先于起轮次"的时序一致），会短暂占住轮询；因此必须有时限、且失败不得让循环卡死。
- OGG/OPUS 不做手工转码：`faster-whisper` 经 PyAV 自带解码能力直吃 OGG（Spike 已实测确认）。
- 转写 MUST 开启 `vad_filter=True`：不开 VAD 时静音/噪音会被 Whisper **幻觉成真实文本**（实测输出 `'You'`）并被当作"用户说的话"进入对话——属正确性缺陷，不是体验问题。

**Scale/Scope**: 单用户个人助手；语音消息低频、单条以 1 分钟以内为主。不涉及并发多用户。

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| 原则 | 判定 | 依据 |
|------|------|------|
| **I. 单主线开发（Telegram-Only）** | ✅ 通过 | 改动落在 `channels/telegram.rs`（Telegram 专属）、`config.rs`（共享，加开关）、`media-gateway/`（媒体能力面）。**不触碰** `src/api/**`、`src/channels/ilink/**`、前端。新增环境变量同步到 `.env.example`。验收对象是 Telegram 端行为。 |
| **II. 显式提交** | ✅ 通过 | 本 plan 不产生任何 `git add`/`commit`；提交留给用户显式指示。 |
| **III. 绿色基线** | ✅ 通过 | 任务清单收尾包含 `cargo test` 全过、`cargo check --all-targets` 零警告、`cargo fmt --all -- --check` 无 diff。**无 schema 变更** → 不需要 `sa-mem check`。Python 侧跑 `python -m unittest discover`。 |
| **IV. 真实库不可污染** | ✅ 通过 | 单测用 `Database::in_memory()`；端到端验证在 quickstart 中强制 `SMART_ASSISTANT_DB` 指向临时库，并要求验证后清理 `messages`/`memories`/`memory_vectors`。 |
| **V. 最小改动与测试分离** | ✅ 通过 | 不重构既有函数（`inbound_text` 保留原语义，仅新增解析分支）；新单测写进既有的独立测试文件，不塞进逻辑文件末尾；不改动未要求触碰的通道。 |

**Gate 结论**：无违规，无需 Complexity Tracking。

### 复检（Phase 1 设计完成后）

设计产物（`research.md` / `data-model.md` / `contracts/` / `quickstart.md`）复核后仍全部通过：

- 设计未引入新 crate、新数据库列、新迁移 → 原则 III 的附加门（`sa-mem check`）不触发。
- 错误分流设计把"能力不可用"隔离在**不产生面向用户内容**的路径上 → 与 FR-006/013 一致，未用话术掩盖缺陷。
- quickstart 明确要求临时库与事后清理 → 原则 IV 可执行。
- Python 侧新增文件集中在 `media-gateway/gateway/asr/` 与 `tests/test_asr.py`，未改动出图/TTS 既有路径 → 原则 V 一致。

## Project Structure

### Documentation (this feature)

```text
specs/001-telegram-voice-transcription/
├── plan.md                              # 本文件（/speckit.plan 产物）
├── spec.md                              # 规格
├── research.md                          # Phase 0 产物（选型与决策记录）
├── data-model.md                        # Phase 1 产物（实体与内容约定）
├── quickstart.md                        # Phase 1 产物（端到端验证指南）
├── contracts/
│   └── transcribe-api.md                # Phase 1 产物（网关 /transcribe 接口契约）
├── checklists/requirements.md           # /speckit.specify 产物
└── tasks.md                             # /speckit.tasks 产物（非本命令生成）
```

### Source Code (repository root)

```text
backend/
├── src/
│   ├── channels/
│   │   ├── telegram.rs           # 改：TgMessage 增 voice/audio/caption；新增下载+转写+分流；run_loop 时序调整
│   │   ├── telegram_tests.rs     # 改：新增入站语音解析、分流判定、marker 生成的单测
│   │   └── mod.rs                # 不改（复用 AI_DEADLINE_SECS / ts_to_rfc3339 / with_send_retry）
│   └── config.rs                 # 改：新增 VOICE_TRANSCRIBE_ENABLED / VOICE_TRANSCRIBE_TIMEOUT / VOICE_TRANSCRIBE_MAX_SECS
├── .env.example                  # 改：同步上述三个新变量
└── tests/                        # 不改（集成测试保持现状）

media-gateway/
├── gateway/
│   ├── server.py                 # 改：新增 POST /transcribe 路由；/health 增 asr 段
│   ├── config.py                 # 改：新增 asr_engine/asr_model/asr_language/asr_device/asr_compute_type/asr_model_dir/asr_warmup
│   ├── asr/
│   │   ├── __init__.py           # 新：transcribe() 分派 + 引擎可用性探测（importable）
│   │   └── faster_whisper.py     # 新：懒加载 faster-whisper，模型进程内单例
│   └── speech/                   # 不改（SAPI / edge-tts 出语音）
├── tests/
│   ├── test_asr.py               # 新：分派 / 错误映射 / 引擎不可用（用假引擎，零依赖）
│   └── ...                       # 不改
├── .env.example                  # 改：同步 ASR 相关变量
├── README.md                     # 改：补「语音识别」一节（uv 装 3.13、av<19 锁版、模型下载、排错）
└── .venv/                        # 新（本地，不入库）：必须进 .gitignore
```

**Structure Decision**: 沿用仓库既有的「Rust 内核 + 本机 Python 媒体网关」双层结构，不新建工程、不新增 crate/依赖层。转写的**网络调用**（下载 + POST 网关）留在 `channels/telegram.rs`（Telegram 专属、且与既有 `media_generate` 同构）；转写的**引擎与模型**留在网关的 `gateway/asr/`（与 `gateway/imaging/`、`gateway/speech/` 同构，遵循 README 声明的扩展点约定）。测试文件遵循宪法原则 V：Rust 侧进既有独立文件，Python 侧进 `tests/`。

## 关键设计（Phase 1 收敛结论）

### 入站分流（`run_loop` 时序调整）

现状：`inbound_text(&message)` 是同步纯函数，在不新鲜度分级**之前**调用。

调整后顺序（文本/贴纸行为完全不变）：

```text
1. 白名单过滤（不变）
2. 取 sent_at → classify_inbound（前移）
3. TooOld → 存档：文本/贴纸按现状；语音落一条 [对方发来一条语音（已过期）]
           （**不触发转写**，避免为不会被回复的消息付 ASR 成本）
4. 新鲜 → resolve_inbound_text(client, base, message):
     text    → Some(原文)
     sticker → Some(占位描述)            # 现状不变
     voice/audio → 转写（见下），产出 VoiceOutcome
5. 落库 → 起轮次（不变）
```

### 转写结果分流（VoiceOutcome）

| 网关结果 | 归类 | 落库内容 | 用户看到 |
|---|---|---|---|
| `200` 且 `text` 非空 | 成功 | `[语音] <转写文本>` | 助手的正常回复（**不回显**转写，FR-010） |
| `200` 且 `text` 为空（VAD 判定整条无人声） | 听不清 | `[对方发来一条语音，但没能听清内容]` | 模型自然生成的"没听清/没明白"（FR-004/005） |
| `duration` > `VOICE_TRANSCRIBE_MAX_SECS`（**前置，不下载不转写**） | 内容层面 | `[对方发来一条语音，但内容太大没能听清]` | 同上（FR-003/005） |
| `file_size` > 20 MB（**前置，不下载**） | 内容层面 | `[对方发来一条语音，但内容太大没能听清]` | 同上（FR-003/005） |
| `4xx`（解码失败/音频过长/无语音） | 内容层面 | `[对方发来一条语音，但没能听清内容]` | 同上（FR-003/005） |
| `5xx` / 连接失败 / 超时 | **系统故障** | **不落库、不起轮次** | 无回复；`tracing::error!` 记录（FR-006/013） |
| `VOICE_TRANSCRIBE_ENABLED` 未开 | **能力未启用** | **不落库、不起轮次** | 无回复；启动时即打印明确告警 + 每次触达记 error（FR-013） |

> 两条**前置**判定必须在下载之前完成：`duration` 超限若不前置，长语音会一路跑到超时，表现为"用户长时间收不到任何东西 + 最终静默"，正是 FR-003 明令禁止的"长时间无响应"（research.md D10）。

分界线一句话：**4xx 是"这条音频的问题"（交给模型用人的方式回应），5xx/连接失败是"我们自己的问题"（缺陷，不伪装）。**

### 能力可用性（FR-013）

- **启动即可知**：`sa-link` 启动时，若 `VOICE_TRANSCRIBE_ENABLED` 已开，则对网关 `/health` 探测一次，把 `asr` 段（引擎名 + 是否可导入）打进日志；未开则打印一行"语音转写未启用（VOICE_TRANSCRIBE_ENABLED=0）"，让能力状态在启动时就明确，而不是等用户发语音才炸。
- **网关侧自检**：`/health` 增加 `asr: {"engine": "...", "importable": true|false}`，用 `importlib.util.find_spec` 判定（**不加载模型**，避免健康检查触发 GB 级加载）。
- **运行期**：引擎不可导入 → `/transcribe` 返回 `503 + kind=engine_unavailable`，Rust 走系统故障路径。

### 边界处理

- **caption 并存（FR-012）**：音频文件带 `caption` 时，落库内容为 `<caption>\n<转写文本>`；转写失败但有 caption 时，caption 仍作为消息主体，追加标记。
- **重复投递（FR-007）**：沿用既有 `getUpdates` 游标（`save_cursor`）与 `begin_turn` 的"同会话同时一轮"约束，不新增去重逻辑。
- **超限（FR-003）**：两道前置拦截，都**不下载**——`duration` 超过 `VOICE_TRANSCRIBE_MAX_SECS`（默认 120s）、`file_size` 超过 20 MB（Bot API 硬限）。二者均判为内容层面失败 → 交模型自然回应。时长前置同时消除"长语音跑到超时"这一 FR-003 冲突（research.md D10）。
- **无人声 / 幻觉（FR-004）**：网关 MUST 用 `vad_filter=True` 转写，无语音段返回空 `text`；Rust 侧据此判"听不清"。不得依赖"Whisper 自己输出空串"——不开 VAD 时静音会被幻觉成 `'You'`（Spike 实测）。
- **等待期状态（FR-005 假设）**：沿用既有 `send_typing`，不新增"正在听"文案。
- **不回显（FR-010）**：`[语音] ` 前缀只进 `messages.content`（供模型与记忆使用），**不得**回帖给用户；Rust 侧不新增任何"识别结果：…"类输出。

## Complexity Tracking

> 无宪法违规，本节留空。
