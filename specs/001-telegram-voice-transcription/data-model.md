# Phase 1 Data Model: Telegram 语音消息识别

**无数据库 schema 变更**（见 research.md D4）。本文件描述的是**报文结构、内存中的中间实体、内容约定与配置项**，以及它们之间的转换规则。

---

## 1. 入站报文实体（Telegram → Rust）

### 1.1 `TgMessage`（既有类型，本次扩展字段）

| 字段 | 类型 | 新增 | 说明 |
|---|---|---|---|
| `text` | `Option<String>` | 否 | 文本正文（语音消息没有） |
| `sticker` | `Option<TgSticker>` | 否 | 贴纸（既有） |
| `chat` | `TgChat` | 否 | 至少含 `id` |
| `date` | `Option<i64>` | 否 | 消息发出的 Unix 秒（世界时刻） |
| `voice` | `Option<TgVoice>` | **是** | 语音条（按住说话录的，OGG/OPUS） |
| `audio` | `Option<TgAudio>` | **是** | 音频文件（用户上传的音乐/录音文件） |
| `caption` | `Option<String>` | **是** | 音频文件可带的文字说明（FR-012） |

**校验/优先级规则**：
- 解析顺序 MUST 为 `text` → `sticker` → `voice` → `audio`，与现有 `inbound_text` 的"文本优先"语义一致。
- 音频不带来 `caption` 时，`caption` 为 `None`；`voice`（语音条）在 Telegram 上本身不带 caption，因此 `caption` 实际只与 `audio` 同时出现。
- 非文本且非语音/音频/贴纸的类型（图片、文件、视频……）MUST 保持既有行为：忽略（返回 `None`）。

### 1.2 `TgVoice` / `TgAudio`（新增）

| 字段 | 类型 | `TgVoice` | `TgAudio` | 用途 |
|---|---|---|---|---|
| `file_id` | `String` | ✅ | ✅ | `getFile` 的入参 |
| `duration` | `Option<i64>` | ✅ | ✅ | **超过 `VOICE_TRANSCRIBE_MAX_SECS` 则不下载不转写**（前置拦截，FR-003 / D10）；未超时则仅记录与排错 |
| `mime_type` | `Option<String>` | ✅ | ✅ | POST 给网关的 `Content-Type` |
| `file_size` | `Option<i64>` | ✅ | ✅ | **超过 20 MB 不下载**（Bot API 硬限，FR-003） |
| `file_name` | `Option<String>` | — | ✅ | 仅日志 |
| `title` | `Option<String>` | — | ✅ | 仅日志（`audio` 可带标题） |

**`file_size` / `duration` 缺省时**：不拦截，尝试下载/转写；由下载或转写本身的失败/超时兜底。

---

## 2. 转写侧实体（Rust ↔ 网关）

### 2.1 网关 `POST /transcribe` 契约（摘要，全文见 `contracts/transcribe-api.md`）

请求：原始音频字节（非 multipart），`Content-Type` 取入站报文的 `mime_type`，缺失时 `voice` 回退 `audio/ogg`、`audio` 回退 `audio/mpeg`。

响应（`200`）：

```json
{ "text": "今天晚上想吃什么", "language": "zh", "duration": 3.2 }
```

- `text` 可为**空串** → 表示"音频里没听出内容"（不是错误）。
  - **空串的唯一合法来源是 VAD 判定整条音频无人声**（网关 MUST 以 `vad_filter=True` 转写）。不得依赖 Whisper 自然输出空串——Spike 实测：静音不开 VAD 会输出 `'You'`（`no_speech_prob=0.862`），即"幻觉文本"，会把凭空捏造的内容当成用户说的话。
- `language` / `duration` 均为可选、仅供日志。

### 2.2 内存实体 `VoiceOutcome`（Rust 内部判定，本特性的核心）

新增枚举，由「HTTP 状态码 + 响应体」映射而来（D6/D7）：

| 变体 | 触发条件 | 载体数据 | 后续动作 |
|---|---|---|---|
| `Transcribed { text }` | `200` 且 `text` 去空白后非空 | 转写文本 | 落库 `[语音] <text>`，起轮次 |
| `Unintelligible` | `200` 但 `text` 去空白后为**空**（VAD 判定整条无人声）；或 `422`（无语音/空音频） | 无 | 落库"没听清"标记，起轮次（交模型回应） |
| `Rejected { reason }` | **前置**：`duration > VOICE_TRANSCRIBE_MAX_SECS`、`file_size > 20MB`（见 §5）；**响应层**：`400` / `413` / `415` | 失败原因（日志） | 落库"没听清"标记，起轮次（交模型回应） |
| `Skipped { reason }` | 未启用 / 连接失败 / 超时 / `500` / `503` | 失败原因（日志） | **不落库、不起轮次**；`tracing::error!` |

**不变式（MUST）**：
- `Skipped` MUST NOT 产生任何面向用户的文本，也 MUST NOT 写入 `messages`。
- `Unintelligible` / `Rejected` MUST NOT 让助手基于猜测内容编造回复。
- 只有 `Transcribed` / `Unintelligible` / `Rejected` 会起轮次。
- `Skipped` MUST 只由**系统自身的问题**触发。任何"这条音频太大/太长"都必须走 `Rejected`（否则会把用户的语音长度伪装成程序缺陷，反之亦然）。因此超时归 `Skipped` 的前提是：合法的时长上限已被**前置拦截**挡在超时之前（D10 的三个数成对配置）。

---

## 3. 时间线内容约定（`messages.content`）

无列变更，靠**内容形态**表达来源与失败类型（D4/D5）：

| 场景 | 落库内容 | 理由 |
|---|---|---|
| 转写成功 | `[语音] <转写文本>` | FR-002（当普通消息用）+ FR-009（可区分来源）+ FR-010（不回显给用户） |
| 音频文件带 caption 且转写成功 | `<caption>\n[语音] <转写文本>` | FR-012（caption 不得被覆盖或丢弃） |
| 音频文件带 caption 但转写失败 | `<caption>\n[对方发来一条语音，但没能听清内容]` | caption 仍是消息主体，失败信息追加 |
| 听不清 / 解码失败 | `[对方发来一条语音，但没能听清内容]` | FR-004/005：由模型自然生成回应 |
| 超限（`duration` 超 `VOICE_TRANSCRIBE_MAX_SECS` 或 `file_size` > 20 MB，未下载） | `[对方发来一条语音，但内容太大没能听清]` | FR-003：助手自然告知无法处理 |
| 过期语音（`TooOld`，不转写） | `[对方发来一条语音（已过期）]` | 存档但不回复；避免为历史消息付 ASR 成本（D8） |

**不变式（MUST）**：
- 以上文本 MUST NOT 被当作"面向用户的话术模板"——它们是**给模型看的情境描述**，最终措辞一律由模型生成（FR-005）。
- 成功路径 MUST 只影响本条消息的 `content`，MUST NOT 改变 `role`、`created_at`、顺序或任何记忆沉淀路径。

---

## 4. 配置实体（环境变量）

### 4.1 Rust 侧（`backend/.env`，MUST 同步到 `backend/.env.example`）

| 变量 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `VOICE_TRANSCRIBE_ENABLED` | bool（`1`/`true`） | `false` | 入站语音转写总开关。关闭 = 能力未启用 → `Skipped` + 启动告警（FR-013） |
| `VOICE_TRANSCRIBE_TIMEOUT` | u64 秒 | `180` | 单次转写硬超时；超时归 `Skipped`（系统故障） |
| `VOICE_TRANSCRIBE_MAX_SECS` | u64 秒 | `120` | 语音时长的**前置上限**：超过则不下载、不转写 → `Rejected`（内容层面，FR-003） |

三个数 MUST 成对配置（D10，依据 Spike 实测 RTF ≈ 0.7x）：`MAX_SECS` 决定最坏转写耗时，`TIMEOUT` 必须**大于**该耗时。默认 120s ÷ 0.7 ≈ 84s < 180s ✅。若把 `MAX_SECS` 调到 300，则 `TIMEOUT` 也要相应调到 ≥ 450，否则合法语音会撞超时并被误判成系统故障。

复用既有变量：`MEDIA_GATEWAY_URL`（转写端点拼在它下面）。**注意**：不复用 `MEDIA_GATEWAY_ENABLED` —— 那个开关管的是「是否把 `generate_image`/`generate_voice` 工具下发给模型」（出站），与入站转写是两件事，混用会让"关掉出图工具"顺手关掉语音转写。

### 4.2 网关侧（`media-gateway/.env`，MUST 同步到 `media-gateway/.env.example`）

| 变量 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `ASR_ENGINE` | 枚举 | `faster-whisper` | ASR 引擎；留空/`none` 表示网关不提供转写 |
| `ASR_MODEL` | 字符串 | `small` | 模型规格或本地路径（`tiny`/`base`/`small`/`medium`/`large-v3`…） |
| `ASR_LANGUAGE` | 字符串 | `auto` | `auto` 或语言码（`zh`/`en`…）；`auto` 交给模型自判 |
| `ASR_DEVICE` | 字符串 | `cpu` | `cpu` / `cuda` |
| `ASR_COMPUTE_TYPE` | 字符串 | `int8` | CPU 上建议 `int8`；GPU 可用 `float16` |
| `ASR_MODEL_DIR` | 路径 | 空 | 模型缓存/本地目录；空则用引擎默认（HuggingFace 缓存） |
| `ASR_WARMUP` | bool | `true` | 网关启动时用**后台线程**预热模型（`ASR_ENGINE` 非空时）。Spike 实测 `small` 冷加载 **21~46s**，不预热则第一条语音要等这么久（还会吃掉转写超时的余量，见 D10） |

**安装约束（非环境变量，但属同一份配置事实）**：依赖 MUST 锁 `av<19`（`av 19.x` 删除了 `av.open(metadata_errors=...)`，faster-whisper 1.2.1 仍在传 → 装上即崩）；解释器 MUST 为 uv 安装的 CPython 3.13。

---

## 5. 状态流转（单条入站语音消息）

```text
收到 update(message)
  │
  ├─ 白名单外 ────────────────────────────────► 忽略（既有行为）
  │
  ├─ 无 text/sticker/voice/audio ─────────────► 忽略（既有行为）
  │
  ├─ classify_inbound == TooOld
  │     └─► 落库 [对方发来一条语音（已过期）] ──► 结束（不回复、不转写）
  │
  └─ Fresh
        │
        ├─ VOICE_TRANSCRIBE_ENABLED == false ─► Skipped：记 error ──► 结束（不落库）
        │
        ├─ duration > MAX_SECS ────────────────► Rejected：落库超限标记 ──► 起轮次（**不下载**）
        │
        ├─ file_size > 20MB ──────────────────► Rejected：落库超限标记 ──► 起轮次（**不下载**）
        │
        └─ getFile → 下载 → POST /transcribe (≤ VOICE_TRANSCRIBE_TIMEOUT)
              │
              ├─ 200 + 非空 text ──────────────► Transcribed ──► 落库 [语音] …  ──► 起轮次
              ├─ 200 + 空 text（VAD 无人声）/ 422 ─► Unintelligible ─► 落库没听清标记 ─► 起轮次
              ├─ 400 / 413 / 415 ─────────────► Rejected ──────► 落库没听清标记 ─► 起轮次
              └─ 连接失败 / 超时 / 500 / 503 ─► Skipped ───────► 记 error，不落库 ─► 结束
```

> 两道**前置**判定（`duration`、`file_size`）都排在下载之前。`duration` 前置是本特性修 FR-003 冲突的关键：若不前置，长语音会一路跑到转写超时才静默失败，正是规格禁止的"长时间无响应"。

**起轮次之后**：与文字消息完全同构 —— 落库先于起轮次、同会话同时一轮、运行中插话、失败轮次持久化重试，全部复用既有机制，无新增路径。
