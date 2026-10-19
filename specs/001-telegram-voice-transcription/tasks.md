---

description: "Task list for Telegram 语音消息识别"

---

# Tasks: Telegram 语音消息识别

**Input**: Design documents from `/specs/001-telegram-voice-transcription/`

**Prerequisites**: [plan.md](./plan.md)（技术栈与结构）、[spec.md](./spec.md)（用户故事与优先级）、[research.md](./research.md)（决策 D1–D10）、[data-model.md](./data-model.md)（实体与内容约定）、[contracts/transcribe-api.md](./contracts/transcribe-api.md)（接口契约）、[quickstart.md](./quickstart.md)（验证场景）

**Tests**: 规格未主动要求 TDD，但本仓库**宪法原则 III（绿色基线）**要求 `cargo test` 全过，因此下方包含测试任务。它们不是可选项——是仓库的可合入门槛。

**Organization**: 按用户故事分阶段，每个故事可独立实现、独立验证、独立交付。

**任务总数**: 32 条（T001–T031，含 T004b）。分布：Setup 5 / Foundational 9 / US1 3 / US2 6 / US3 2 / Polish 7。

## Format: `[ID] [P?] [Story] Description`

- **[P]**: 可并行（不同文件，且不依赖未完成任务）
- **[Story]**: 所属用户故事（US1 / US2 / US3）
- 每个任务都带**确切文件路径**

## Path Conventions

本仓库为「Rust 内核 + 本机 Python 媒体网关」双层结构（见 plan.md）：

- Rust 后端：`backend/src/**`、`backend/tests/**`
- Python 网关：`media-gateway/gateway/**`、`media-gateway/tests/**`
- 规格文档：`specs/001-telegram-voice-transcription/**`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: 两侧的配置项与示例文件——所有故事都要读这些开关。

- [x] T001 [P] 在 `media-gateway/gateway/config.py` 的 `Settings` 中新增 ASR 字段并写入 `from_env()`：`asr_engine`（默认 `faster-whisper`，空/`none` 表示不提供转写）、`asr_model`（默认 `small`）、`asr_language`（默认 `auto`）、`asr_device`（默认 `cpu`）、`asr_compute_type`（默认 `int8`）、`asr_model_dir`（默认空）、`asr_warmup`（默认 `true`）；同步更新文件头部的环境变量说明注释
- [x] T002 [P] 把 T001 的 7 个 ASR 变量同步到 `media-gateway/.env.example`（含注释与默认值）
- [x] T003 [P] 在 `backend/src/config.rs` 的 `Config` 新增字段并写入 `init_from_env()`：`voice_transcribe_enabled: bool`（读 `VOICE_TRANSCRIBE_ENABLED`，按仓库既有写法 `v == "1" || v.eq_ignore_ascii_case("true")`，默认 `false`）、`voice_transcribe_timeout: u64`（读 `VOICE_TRANSCRIBE_TIMEOUT`，默认 `180`）、`voice_transcribe_max_secs: u64`（读 `VOICE_TRANSCRIBE_MAX_SECS`，默认 `120`）；**不复用** `MEDIA_GATEWAY_ENABLED`
- [x] T004 [P] 把 T003 的 3 个变量同步到 `backend/.env.example`，注释说明「入站语音转写总开关」与「与 MEDIA_GATEWAY_ENABLED 的区别」，并写明 `TIMEOUT` 与 `MAX_SECS` 必须成对配置（`TIMEOUT > MAX_SECS × 1.0 + 60`，按 RTF 最坏 1.0x 与冷加载最坏 46s 算，见 research.md D10）
- [x] T004b [P] 把 `media-gateway/.venv/` 追加到仓库根 `.gitignore`（ASR 依赖装在本地 venv 里，绝不能入库）

**Checkpoint**: 两侧配置就位，后续任务可直接读取。

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: 网关端点（契约）与 Rust 侧的报文解析 / HTTP 客户端 / 判定映射。**任何用户故事开工前必须完成**。

**⚠️ CRITICAL**: 本阶段完成前，US1/US2/US3 都无法开始。

- [x] T005 [P] 新建 `media-gateway/gateway/asr/__init__.py`：实现 `transcribe(audio: bytes, content_type: str, config: Settings) -> tuple[str, str | None, float | None]` 的**引擎分派**（按 `asr_engine`）、`engine_importable(engine: str) -> bool`（用 `importlib.util.find_spec`，**不得加载模型**）、以及**错误到 HTTP 状态码的映射**：无法解码→`400`、音频过长→`413`、格式不支持→`415`、无有效语音→`422`、引擎未安装/加载失败→`503`（`kind="engine_unavailable"`）、其他内部错误→`500`。**分派层 MUST 强制向引擎传 `vad_filter=True`**（Spike 实测：不开 VAD 时静音会被幻觉成 `'You'` 等文本，导致"空串⇒没听清"的判据失效）。契约见 `contracts/transcribe-api.md`
- [x] T006 [P] 新建 `media-gateway/gateway/asr/faster_whisper.py`：**懒加载** `faster_whisper`（导入失败即抛"引擎不可用"，不得让进程启动失败）、进程内模型单例（首次调用才加载 `asr_model`）、`warmup(config)`（供启动后台预热调用，失败只记日志不抛）、把引擎异常归类为 T005 定义的错误种类；读取 `asr_language`/`asr_device`/`asr_compute_type`/`asr_model_dir`；文件顶部注释写明依赖 MUST 锁 `av<19`（`av 19.x` 移除 `av.open(metadata_errors=...)`，与 faster-whisper 1.2.1 不兼容，装上即崩）
- [x] T007 改 `media-gateway/gateway/server.py`：`do_POST` 增加 `POST /transcribe` 分支——按 `Content-Length` 读**原始字节**（非 JSON）、取 `Content-Type` 头、返回 `200 {"text","language","duration"}`；`/health` 响应增加 `asr: {"engine": <asr_engine>, "importable": <engine_importable()>, "model_loaded": <bool>}`；启动时若 `asr_warmup` 为真且引擎非空，**用后台线程**调用 `warmup()`（MUST NOT 阻塞监听启动）；**不得改动** `/generate/image`、`/generate/voice` 的既有分支与响应结构（依赖 T005、T006）
- [x] T008 [P] 新建 `media-gateway/tests/test_asr.py`（`unittest`，零依赖，用**假引擎**注入，不要求真的装 faster-whisper）：覆盖 `contracts/transcribe-api.md` 的 **9 条**测试契约——`ASR_ENGINE=none`→503、引擎不可导入→503、空串→200+`text==""`、解码失败→400、过长→413、正常→200 透传、`/health` 不可导入时 `asr.importable == false` 且不抛异常、**假引擎收到的 kwargs 含 `vad_filter=True`**、**`asr_warmup=true` 且预热抛异常时进程仍可启动且 `/health` 为 200**
- [x] T009 [P] 改 `backend/src/channels/telegram.rs`：`TgMessage` 增加 `voice: Option<TgVoice>`、`audio: Option<TgAudio>`、`caption: Option<String>`；新增 `TgVoice`（`file_id: String`、`duration: Option<i64>`、`mime_type: Option<String>`、`file_size: Option<i64>`）与 `TgAudio`（同前 + `file_name: Option<String>`、`title: Option<String>`）结构体；解析优先级保持 `text` → `sticker` → `voice` → `audio`
- [x] T010 改 `backend/src/channels/telegram.rs`：新增入站语音 HTTP 客户端三件套——`getFile` 取 `file_path`、从 `https://api.telegram.org/file/bot<token>/<file_path>` 下载字节、`POST {MEDIA_GATEWAY_URL}/transcribe` 送原始字节；客户端 MUST `.no_proxy()`（沿用 `media_generate()` 既有做法），超时取 `voice_transcribe_timeout`，返回 `(status: u16, body: String)` 供上层判定
- [x] T011 改 `backend/src/channels/telegram.rs`：新增 `VoiceOutcome` 枚举（`Transcribed { text }` / `Unintelligible` / `Rejected { reason }` / `Skipped { reason }`）与纯函数 `voice_outcome(status: u16, body: &str) -> VoiceOutcome`，**严格按 `contracts/transcribe-api.md` 的状态码语义**：200+非空→`Transcribed`；200+空串（去空白，仅剩标点亦视为空）或 422→`Unintelligible`；400/413/415→`Rejected`；500/503→`Skipped`。注意**前置**拦截（时长/体积）不走此函数，由 T018 直接产出 `Rejected`
- [x] T012 [P] 改 `backend/src/channels/telegram_tests.rs`：新增单测覆盖 T009 的报文解析（`voice`/`audio`/`caption` 各字段、缺失字段为 `None`、解析优先级）与 T011 的映射（200 非空 / 200 空串 / 200 仅标点 / 422 / 400 / 413 / 415 / 500 / 503 各一例；并断言"仅标点"归 `Unintelligible`，而"嗯""好"这类简短应答归 `Transcribed`）
- [x] T013 改 `backend/src/channels/telegram.rs` 的 `run_loop`：把 `classify_inbound` 判定**前移到内容解析之前**；`TooOld` 的语音落 `[对方发来一条语音（已过期）]` 后跳过，**不触发转写**；文本 / 贴纸路径的最终行为 MUST 保持不变（依赖 T009）

**Checkpoint**: 端点、解析、客户端、判定映射齐备——三个用户故事可以并行开工。

---

## Phase 3: User Story 1 - 说一句就能聊 (Priority: P1) 🎯 MVP

**Goal**: 用户发一条内容清晰的语音，助手像收到文字一样理解并回复；用户不会看到转写文字；语音消息在时间线里以 `[语音] <文本>` 形态存在。

**Independent Test**: 真机发一条 5 秒中文语音（如"今天晚上想吃什么"）→ 助手给出相符回复、**不回显转写**；临时库中该条 `content` 形如 `[语音] 今天晚上想吃什么`；随后发文字延续话题，助手体现上下文连续。

### Implementation for User Story 1

- [x] T014 [US1] 改 `backend/src/channels/telegram.rs`：接线成功路径——新鲜语音/音频 → T010 下载并转写 → `voice_outcome` 为 `Transcribed` → 组内容（无 caption 时 `[语音] <text>`；有 caption 时 `<caption>\n[语音] <text>`，见 `data-model.md` §3）→ 走既有 `add_message_at` 落库 → `begin_turn` + `spawn_turn`（FR-001/002/009/010/012）。**MUST NOT 新增任何授权、白名单或开关检查**——白名单与 `classify_inbound` 已在既有路径上完成，语音走同一条路（FR-011）
- [x] T015 [P] [US1] 改 `backend/src/channels/telegram_tests.rs`：新增内容组装单测——无 caption 产出 `[语音] <text>`、有 caption 时 caption 在前且不被覆盖、转写文本含首尾空白时按去空白后判定非空；并断言成功路径产出的内容**只用于落库**、不带任何面向用户的回显字段（FR-010）
- [x] T016 [US1] 按 `quickstart.md` 场景 A（说一句就能聊）与场景 E（caption 并存）做独立验证，记录实测结果；**同时验证 SC-001**——对同内容的语音与文字各发一次并计时，语音总耗时 MUST ≤ 文字总耗时的 2 倍（在 quickstart 中记录两次的实际秒数）

**Checkpoint**: US1 单独可用——语音消息从"被丢弃"变成"能对话"，MVP 达成。

---

## Phase 4: User Story 2 - 听不清就像人一样说一句 (Priority: P2)

**Goal**: 听不清时由**模型**用自然语言回应（非固定话术）；转写能力不可用则走系统故障路径——不落库、不起轮次、不伪装成"没听清"。

**Independent Test**: 发一段无人声音频 → 助手回一句自然生成的"没听清"（两次措辞应不完全相同）、临时库落 `[对方发来一条语音，但没能听清内容]`；停掉网关再发语音 → **无任何回复、无任何话术**、日志有 `error` 记录、临时库**无新增行**。

### Implementation for User Story 2

- [x] T017 [US2] 改 `backend/src/channels/telegram.rs`：内容层面失败（`Unintelligible` / `Rejected`）→ 落 `[对方发来一条语音，但没能听清内容]`（有 caption 时 caption 在前）→ **照常起轮次**，由模型生成回应；仓库内 MUST NOT 内置任何固定话术字符串（FR-004/005）
- [x] T018 [US2] 改 `backend/src/channels/telegram.rs`：**两道前置拦截**（均在下载之前，直接判 `Rejected`，不发起 `getFile`/下载/转写）——① `duration` 超过 `voice_transcribe_max_secs`（默认 120s）；② `file_size` 超过 20 MB（Telegram Bot API `getFile` 硬限）→ 落 `[对方发来一条语音，但内容太大没能听清]`（FR-003）。时长前置是**修 FR-003「不能长时间无响应」的必需项**：缺少它时，长语音会一路跑到转写超时，表现为用户长时间收不到任何东西 + 最终静默（research.md D10）。拦截判定 MUST 写成可单测的纯函数
- [x] T019 [US2] 改 `backend/src/channels/telegram.rs`：能力层面失败（`voice_transcribe_enabled == false` / 连接失败 / 超时 / `500` / `503`）→ `Skipped`：`tracing::error!` 记录 `chat_id` 与原因，**不调用 `add_message_at`、不 `begin_turn`**，MUST NOT 产生任何面向用户的文本（FR-006/013）。**超时归 `Skipped` 的前提是 T018 的时长前置已生效**——否则合法语音会被误判成程序故障
- [x] T020 [US2] 改 `backend/src/channels/telegram.rs`：启动自检——`run_loop` 启动时，若转写已启用则请求网关 `/health` 并把 `asr.engine` / `asr.importable` / `asr.model_loaded` 打进日志；若未启用则明确打印"未启用（VOICE_TRANSCRIBE_ENABLED=0）"；探测失败不得阻止长轮询启动（FR-013）
- [x] T021 [P] [US2] 改 `backend/src/channels/telegram_tests.rs`：新增分流单测——断言 ①`Unintelligible`/`Rejected` 产出各自正确的 marker 文本；②`Skipped` 产出 `Option<String>` 为 `None`（即**不产生任何可落库文本、不起轮次**，FR-006）；③时长超限与体积超限的前置判定被拦下且**不调用下载**（用可注入的假下载记录调用次数，FR-011/003）；④两种超限产出的 marker 一致
- [x] T022 [US2] 按 `quickstart.md` 场景 B（听不清）/ 场景 C（能力不可用）/ 场景 F（超限，含**一条超过时长上限的语音**）做独立验证，记录实测结果

**Checkpoint**: US1 与 US2 均可独立工作——正常语音能聊，异常情形不静默、不伪装。

---

## Phase 5: User Story 3 - 连续说也不掉链子 (Priority: P3)

**Goal**: 连发多条语音、或助手在回复中用户又补一条语音时，消息按发送顺序并入同一轮，不丢、不重复。

**Independent Test**: 快速连发两条相关语音（"我明天要出差" + "大概去三天"）→ 助手一次回复同时考虑两条；临时库中两条按序存在；同一条语音被重复投递时不产生两次回复。

### Implementation for User Story 3

- [x] T023 [US3] 改 `backend/src/channels/telegram.rs`：确认并加固顺序与并发约束——转写调用的失败/超时 MUST 在有界时间内返回且**不得 panic**（否则会打断本条之后的 update 处理）；落库仍先于起轮次，同会话仍由 `begin_turn` 保证同时只跑一轮；重复投递去重沿用既有 `save_cursor` 游标，**不新增去重逻辑**（FR-007）。同时确认多段语音的**处理顺序与生成内容的顺序一致**（转写是串行的，不得并发乱序落库）
- [x] T024 [US3] 按 `quickstart.md` 场景 D（连续说）做独立验证：连发两条语音 → 一次回复覆盖两条；并核对临时库两条按 `created_at` 有序。**若 T023 顺序断言无法自动化，则在本任务记录手工核对结果，并在下一轮补充自动化断言**

**Checkpoint**: 三个故事全部独立可用。

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: 文档、绿色基线、合规核对、端到端走查。

- [x] T025 [P] 改 `media-gateway/README.md`：补「语音识别」一节——`/transcribe` 用法、ASR 环境变量表、**安装前提**（`uv python install 3.13` + `uv pip install faster-whisper "av<19"`，并说明为何不能用系统 3.15 与 `py -3.13`）、首次拉模型（含 `HF_ENDPOINT` 镜像兜底）、`/health` 的 `asr` 段说明、**冷启动预热与 `ASR_WARMUP`**、`vad_filter` 的作用
- [x] T026 [P] 合规核对：`git status` / `git diff --stat` 确认改动**只**落在 `backend/src/channels/telegram.rs`、`backend/src/channels/telegram_tests.rs`、`backend/src/config.rs`、`backend/.env.example`、`gitignore`（补 `.venv`）、`media-gateway/**`、`specs/**`；**未触碰** `backend/src/api/**`、`backend/src/channels/ilink/**`、`frontend/**`（宪法原则 I）
- [x] T027 绿色基线（宪法原则 III）：`cd backend && cargo test`、`cargo check --all-targets`（零警告）、`cargo clippy --all-targets`（零警告）、`cargo fmt --all`（不留 diff）；本特性**无 schema 变更**，故不需要 `sa-mem check`
- [x] T028 网关测试：在 uv 建的 venv 下跑 `media-gateway/.venv/Scripts/python.exe -m unittest discover -v`（`cwd=media-gateway`）全过。**不要用 `py -3.13`**——本机不可用（见 research.md D2）
- [x] T029 端到端走查 `quickstart.md` 全流程，**并完成第 7 节收尾**：临时库删除、确认真实库 `smart_assistant.db` 的 `LastWriteTime` 未被触碰、还原 `backend/.env` 的 `SMART_ASSISTANT_DB`
- [x] T030 [P] **SC-003 抽样验证**：按 quickstart 第 8 节，对 20 组同一内容分别以「语音」与「文字」各发一次，逐组按 SC-003 的判定口径（话题域是否一致）记录一致/不一致，给出 `一致数/20` 结论；≥ 18 为达标。若未达标，记录失败样本并据此回溯是 ASR 误识别还是下游理解问题
- [x] T031 [P] **SC-002 基线复核**：确认改动前的"语音被静默丢弃"基线（100%）与改动后的取值方式一致，并在 quickstart 中写明"0% 静默"的核对方法（发一条语音后临时库**必有一行**新 `messages`，或日志**必有** error 记录——两者必居其一）

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: 无依赖，可立即开始
- **Foundational (Phase 2)**: 依赖 Setup——**阻塞全部用户故事**
- **User Stories (Phase 3–5)**: 均依赖 Foundational 完成；此后可并行（若多人）或按 P1 → P2 → P3 顺序推进
- **Polish (Phase 6)**: 依赖所需用户故事完成

### Task-level Dependencies（关键链路）

```
T001 ─┐
T002  │
      ├─► T005 ─► T007 ─┐
T006 ─┘                 │
                        ├─► T014 (US1) ─► T015, T016
T003 ─┬─► T010 ─────────┘        │
T004 ─┘        │                 └─► T017 (US2) ─► T018 ─► T019 ─► T020 ─► T021, T022
               │                                                              │
T009 ─► T011 ──┴──────────────────────────────────────────────────────────────┴─► T023 (US3) ─► T024
T012 ─► T013 ─────────────────────────────────────────────────────────────────────► T025..T031
```

- `T007` 与 `T014` 之间是**契约边界**：T007 之前，Rust 侧无法真正联调。
- `T014`（US1 成功路径）是 `T017`（US2 失败路径）的**同函数扩展点**——建议同一实现者连续完成，避免同文件冲突。
- `T023`（US3）不是新功能，而是对 T014/T019 的**约束加固 + 验证**，因此排在最后。

### Within Each User Story

- 先实现、再单测、最后按 quickstart 独立验证
- 内容组装（T014/T015）先于失败分流（T017/T021）
- 故事完成后再进下一个优先级

### Parallel Opportunities

- Setup 的 T001–T004 全部 `[P]`，四路并行
- Foundational 中 T005/T006/T008/T009/T012 为 `[P]`（不同文件）；T010 与 T011 改成同文件 `telegram.rs` 内的连续任务（**不再标 `[P]`**——同一文件并行编辑会冲突），T007（依赖 T005+T006）、T013（依赖 T009）亦需等待
- 每个故事内的测试任务（T015/T021）可与其他故事并行

---

## Parallel Example: Foundational（Phase 2）

```bash
# 网关侧三路并行：
Task: "T005 新建 media-gateway/gateway/asr/__init__.py（分派 + 可导入性探测 + 错误映射 + 强制 vad_filter）"
Task: "T006 新建 media-gateway/gateway/asr/faster_whisper.py（懒加载 + 模型单例 + warmup）"
Task: "T008 新建 media-gateway/tests/test_asr.py（9 条契约，假引擎）"

# Rust 侧（telegram.rs 内为连续任务，不并行；测试文件另算一路）：
Task: "T009 backend/src/channels/telegram.rs：TgMessage 增字段与解析"
Task: "T010 → T011 backend/src/channels/telegram.rs：HTTP 客户端 → VoiceOutcome 映射（同文件，串行）"
Task: "T012 backend/src/channels/telegram_tests.rs：解析与映射单测"
```

## Parallel Example: User Story 1

```bash
Task: "T014 [US1] backend/src/channels/telegram.rs：成功路径接线"
Task: "T015 [US1] backend/src/channels/telegram_tests.rs：内容组装单测"
```

---

## Implementation Strategy

### 实现进度（截至 2026-10-19）

- **32 条全部勾选**。其中代码/文档类 26 条（T001–T015、T017–T021、T023、T025–T028）已完成，
  真机验证类 6 条由**用户决定免验**（见下）。
- **已用真实件验证的部分**（证据见 `research.md` 顶部「实测基线」）：
  - 网关侧：真 `media-gateway` 进程 + 真 `faster-whisper`（`media-gateway/.venv`，CPython 3.13.16 /
    `av 18.1.0`）→ `POST /transcribe` 送 9293 字节 OGG/OPUS 得 `200 {"text":"今天晚上想吃什么?"}`；
    静音得 `text==""`（VAD 生效）；坏字节得 `400`；空 body 得 `400`；`/health` 的 `asr` 段在预热后
    `model_loaded` 转 `true`。
  - Rust 侧：`cargo test` 192 + 6 + 3 全过、`cargo clippy --all-targets` 零警告、
    `cargo fmt --all -- --check` 无 diff。
  - 网关侧：`media-gateway/.venv/Scripts/python.exe -m unittest discover` 41 条全过。
- **用户决定免验、由用户后续自行验证**：T016（场景 A/E + SC-001）、T022（场景 B/C/F）、
  T024（场景 D）、T029（端到端 + 收尾）、T030（SC-003 抽样 20 组）、T031（SC-002 基线复核）。
  勾选表示"按用户指示通过"，**不代表已实测**；实测结论请回填本节。
- **T023**：顺序/并发约束由实现结构保证——转写在单条 update 内串行 `await`、落库仍先于起轮次、
  同会话仍由 `begin_turn` 保证单轮、无新增去重逻辑；未额外写成自动化断言。
- **功能默认关闭**：`VOICE_TRANSCRIBE_ENABLED=0`。不显式打开，收到语音只记 error，行为与改动前一致。
- **真机验证步骤**：`backend/.env` 加 `VOICE_TRANSCRIBE_ENABLED=1` → 起
  `media-gateway/.venv/Scripts/python.exe main.py` → 起 `cargo run --bin sa-link`。

### MVP First（仅 User Story 1）

1. Phase 1 Setup（T001–T004）
2. Phase 2 Foundational（T005–T013）——**关键，阻塞全部故事**
3. Phase 3 US1（T014–T016）
4. **STOP & VALIDATE**：按 quickstart 场景 A 独立验证
5. 此时已可交付：语音消息从"被静默丢弃"变成"能对话"

### Incremental Delivery

1. Setup + Foundational → 基础就绪
2. **+ US1** → 验证 → 交付（MVP）
3. **+ US2** → 验证 → 交付（听不清不再石沉大海；能力不可用不再被话术掩盖）
4. **+ US3** → 验证 → 交付（连珠炮式发送不掉链子）
5. 每个故事独立增值、互不破坏

### 实现提示（避免踩坑）

- **先验 T007 再写 Rust 联调**：契约是双边的，端点没起就调不出转写
- **装依赖必须锁 `av<19`**：`av 19.x` 与 faster-whisper 1.2.1 不兼容，装上即崩（Spike 实测）；解释器必须用 `uv python install 3.13`，别用 `py -3.13`
- **`vad_filter=True` 不能省**：省掉它，静音会被幻觉成 `'You'` 之类的真实文本并当作用户发言喂进对话——这是正确性缺陷，不是体验问题（Spike 实测）
- **时长上限必须前置**：T018 的两道拦截都要排在下载之前，否则长语音跑到超时会变成"长时间无响应 + 静默"，违反 FR-003
- **测试环境用 `Database::in_memory()`**：`cargo test` 绝不碰磁盘库（宪法原则 IV）
- **手工验证必须换临时库**：`SMART_ASSISTANT_DB=smart_assistant_voice_test.db`，验完删除并核对真实库时间戳
- **不要内置固定话术**：`[对方发来一条语音，但没能听清内容]` 是给模型看的情境描述，不是用户可见文案；最终措辞一律由模型生成（FR-005）
- **能力不可用绝不落库**：一旦为它写占位消息，就把缺陷伪装成了内容问题（research.md D6）
- **不新增依赖、不加列、不碰 Web/iLink**：见 research.md 末尾「本特性不引入的东西」

---

## Notes

- `[P]` = 不同文件（或同文件互不依赖的函数）且无未完成依赖
- `[Story]` 标签用于把任务追溯回 spec.md 的用户故事
- 每个用户故事都能独立完成与独立验证
- 实现顺序：先让测试失败 → 再实现 → 按 quickstart 验证
- 提交由用户显式指示后才执行（宪法原则 II）；本任务清单**不自动提交**
