# Phase 0 Research: Telegram 语音消息识别

本文件记录 plan 阶段所有需要拍板的技术选择。每条按 **Decision / Rationale / Alternatives considered** 记录。

规格里没有任何 `[NEEDS CLARIFICATION]` 遗留（`/speckit.specify` 阶段已由用户拍板 Q1/Q2），以下全部是 Technical Context 中的未定项。

---

## 实测基线（Spike，2026-10-19）

在动手实现前先跑了一次端到端可行性验证，结论直接改写了下面几处决策。**测得的数据（CPU / int8 / 2.68 秒中文语音）：**

| 项 | 结果 |
|---|---|
| 解释器 | uv 安装的 **CPython 3.13.16**（`uv python install 3.13`） |
| 安装 | 全 wheel、零编译：`faster-whisper 1.2.1` + `ctranslate2 4.8.3` + `av 18.1.0` + `onnxruntime 1.31.0` |
| `tiny` 模型 | 加载 1.15s；2.68s 音频转写 0.36s |
| `small` 模型 | 加载 **21.4s / 46.4s / 43s**（三次实测；随 CPU 负载波动，且 VAD 模型首次初始化也要时间）；2.68s 音频转写 **1.84s / 2.64s / 2.61s / 2.69s（RTF 0.69x ~ 1.00x）** |
| 准确率（small） | 转出 `今天晚上想吃什么?`，与原文**完全一致**；`language=zh (p=1.00)` |
| 模型下载 | HuggingFace **直连成功**，无需镜像 |
| 静音音频（无 VAD） | `text='You'`、`no_speech_prob=0.862` ← **幻觉，不是空串** |
| 静音音频（`vad_filter=True`） | `segments=0`、`text=''`，且耗时 **0.90s（快一倍）** |
| 适配层复验（`io.BytesIO` 直喂 PyAV） | `gateway/asr/faster_whisper.py` 写完后用真实引擎复跑：**9293 字节 OGG/OPUS 直喂成功**、静音得空串、坏字节得 `400`，无需任何转码 |
| 真实 HTTP 端到端（真网关进程） | 起 `media-gateway`（spike venv）后走 HTTP：`/health` 首秒即通且 `asr.importable=true`，预热 43s 后 `model_loaded` 转 `true`；`POST /transcribe` → `200 {"text":"今天晚上想吃什么?","language":"zh","duration":2.68375}`（2.69s）；静音 → `200 {"text":""}`；坏字节 → `400 无法解码音频`；空 body → `400 请求体为空` |

由此产生三条对决策的**硬修正**，已落进 D1 / D2 / D9 / D10 与后续产物：

1. **`av` 必须锁上界（阻塞级）**：`av 19.x` 删除了 `av.open(metadata_errors=...)`，而 `faster-whisper 1.2.1` 仍在传该参数 → `TypeError: open() got an unexpected keyword argument 'metadata_errors'`，**装上即崩**。必须 `av<19`（实测 18.1.0 正常）。
2. **`vad_filter=True` 是必需项**：静音/噪音会被 Whisper 幻觉成真实文本并当作"用户说的话"进入对话——这是**正确性缺陷**，不是体验问题。开 VAD 后无语音段会正确地变成空串，"没听清"判据才成立。
3. **Python 版本指引须改写**：`py -3.13` 在本机**不可用**（`py -0p` 列的两份 StabilityMatrix 解释器实际已不存在，`.local\bin\python3.12/3.13.exe` 亦是断链）；可用路径为 uv。

---

## D1. 转写能力从哪来：网关可选 Python ASR 引擎（`faster-whisper`）

**Decision**: 在 `media-gateway/` 增加 `POST /transcribe`，内部实现可插拔 ASR 引擎，默认引擎 `faster-whisper`（懒加载）。Rust 侧只跟网关做一次 HTTP，与既有 `media_generate()` 同构。

**Rationale**:
- 用户明确选择方案 C（网关加可选 Python ASR 引擎）。
- 与网关既有定位一致：README 声明网关就是"调起本机 AI 软件并把字节交给 Telegram"的那一层，扩展点约定为「加一个后端 → 在新目录实现 → 在分派里挂一行 → `config.py` 加字段」。ASR 与出图 / 出语音同构。
- `faster-whisper` 自带解码能力（PyAV 内嵌 ffmpeg 库），**OGG/OPUS 无需手工转码**，省掉一条 ffmpeg 依赖链（实测：直接吃 `voice` 的 OGG/OPUS 字节即可）。
- 懒加载使网关在没装 ASR 依赖时**仍能正常启动**并继续提供出图 / 出语音能力——失败被限制在 `/transcribe` 一个端点内。

**依赖锁定（Spike 实测得出，必须写进安装说明）**：
- `av` MUST 锁 `av<19`。`av 19.x` 删除了 `av.open(metadata_errors=...)`，而 `faster-whisper 1.2.1` 仍在传该参数 → `TypeError`，**装上即崩**。
- 转写时 MUST 传 `vad_filter=True`。不开 VAD 时静音/噪音会被幻觉成 `'You'` 之类的真实文本（实测 `no_speech_prob=0.862` 仍输出文本），被当作"用户说的话"进入对话——属**正确性缺陷**。开 VAD 后无语音段正确变为空串，FR-004 的"听不清"判据才成立（附带收益：无语音音频耗时减半）。
- 转写成功与否的另一辅助信号是每段的 `no_speech_prob`，只记日志，不作为判据（VAD 已处理该问题）。

**Alternatives considered**:
- **A. Rust 直连 OpenAI 兼容 ASR 接口**（新增 `ASR_API_URL`/`ASR_API_KEY`/`ASR_MODEL`）：Rust 侧改动最小、换供应商只改 env，但依赖外网与密钥；用户选择了本地引擎路线。
- **B. 本机 whisper.cpp 二进制**（`ASR_BIN` + `ASR_MODEL`）：离线、零 Python 依赖，最贴合网关既有"调 ffmpeg"的模式；但需要用户额外下载二进制与 ggml 模型（75 MB ~ 1.5 GB）。
- 两者都不是错的，只是 Spike 已证明 C 在本机**全 wheel、零编译、准确率满贯**，落地成本反而最低。

**遗留风险**（写入安装说明，不阻塞）：
- 首次使用会从 HuggingFace 拉模型（`tiny` 75 MB / `small` 484 MB / `medium` 1.5 GB）。Spike 实测**直连成功**，无需镜像；若用户网络不同，可用 `HF_ENDPOINT=https://hf-mirror.com` 或用 `ASR_MODEL_DIR` 指向本地已下载目录。
- 冷启动（进程内首次转写）需加载模型：`small` 实测 **21~46s**。这是"进程内单例"的必然代价，因此网关 SHOULD 在启动时（`ASR_ENGINE` 非空）用**后台线程预热**模型，让第一条语音不必等这几十秒；`/health` 相应报告 `model_loaded`。
- Windows 上 HF 缓存无法使用符号链接（未开开发者模式），会退化为"占用更多磁盘"，不影响功能。
- `small` 在 CPU 上 RTF = **0.69x ~ 1.00x**（多次实测，随负载波动）：一条 2 分钟语音约需 84~120 秒转写。因此时长上限与转写超时必须**成对配置**（见 D10）。

---

## D2. Python 版本：ASR 场景必须用 CPython 3.13（由 uv 安装）

**Decision**: 网关整体仍保持纯标准库实现；但**开启 ASR 时**，网关必须用 CPython **3.13** 启动，且该解释器 MUST 由 uv 提供。安装与启动方式固定为：

```powershell
uv python install 3.13                        # 实测装得 cpython-3.13.16（独立、不动系统）
uv venv --python 3.13 media-gateway/.venv
uv pip install --python media-gateway/.venv/Scripts/python.exe faster-whisper "av<19"
media-gateway/.venv/Scripts/python.exe media-gateway/main.py
```

**Rationale**（Spike 实测，非推测）：
- 本机默认解释器是 `D:\Python3\python.exe` = **3.15.0a8**，ABI 后缀 `.cp315-win_amd64.pyd`。
- PyPI wheel 覆盖查询结果：`ctranslate2` 4.8.3（faster-whisper 的推理内核）只有 cp39–**cp314**，`onnxruntime` 1.31.0 只有 cp311–cp314 —— **都没有 cp315**。缺 wheel 时 pip 退回**源码编译** `ctranslate2`（C++ + CMake + MSVC，Windows 上通常直接失败）。`faster-whisper` 1.2.1 本身是 `py3` 纯 Python 包，真正的坑全在内核。
- **`py -3.13` 在本机不可用**（原 D2 的结论有误，Spike 已证伪）：`py -0p` 列出的
  `D:\StabilityMatrix(-win-x64)\Data\Assets\Python\cpython-3.1{2,3}.1{2}-...\python.exe` **实际已不存在**，
  属于 py launcher 的**过期注册项**；`C:\Users\61451\.local\bin\python3.12.exe` 与 `python3.13.exe`
  也是断链 shim（`uv` 报 `Failed to spawn the python child process`）。`py -3.13` 直接报
  `No suitable Python runtime found`。
- 因此**不能依赖机器上"看起来有"的解释器**，必须用 uv 现装一个——这同时满足 FR-013 的"稳定的能力"：
  安装步骤可复现、不依赖 StabilityMatrix 的私有目录。
- 网关是纯标准库，跑在 3.13 上不丢任何能力（README 里"3.15 也能跑"是在说明它不依赖编译型包，不是推荐版本）。

**Alternatives considered**:
- 升级/降级系统默认 `python`：影响面过大（StabilityMatrix 等外部工具共用解释器），且没必要。
- 依赖 `py -3.13` 或 StabilityMatrix 的 3.13.12：**实测不可用**（见上），否决。
- 用 3.15 源码编译 ctranslate2：不可控、不可复现，与 FR-013 直接冲突，否决。

---

## D3. 转写在 Rust 侧的落点：`channels/telegram.rs` 内，不新开模块

**Decision**: 入站解析 + 下载 + 调网关 + 分流判定全部写在 `backend/src/channels/telegram.rs`；单测写进既有的 `telegram_tests.rs`。

**Rationale**:
- `TgMessage` / `TgVoice` 等类型是 `telegram.rs` 的模块私有类型；新开兄弟模块需要把它们提升为 `pub(super)`——属于对既有代码的无谓改动。
- 与既有 `media_generate()`（也在 `telegram.rs`）保持同构，读代码的人只需要看一处。
- 宪法原则 V（最小改动）优先于"文件行数"这一主观偏好。

**Alternatives considered**: 新建 `channels/telegram_voice.rs`。更短的单文件，但需要搬迁类型可见性、制造一次纯移动 diff；本特性不需要，故不做（若将来语音逻辑继续膨胀再拆）。

---

## D4. "源自语音"如何保留（FR-009）：内容前缀，不加数据库列

**Decision**: 转写成功落库的内容为 `[语音] <转写文本>`；监听过期语音落 `[对方发来一条语音（已过期）]`；听不清落 `[对方发来一条语音，但没能听清内容]`。

**Rationale**:
- FR-009 要的是"下游能区分来源"，前缀同时满足：模型能看到（可在自我表述里体现"听你说"）、运维能 grep、且**零 schema 变更**。
- 零 schema 变更意味着不需要新增列、不需要迁移、不需要 `sa-mem check`（宪法原则 III 的附加门不触发），也不影响 `messages` 既有读写路径。

**Alternatives considered**:
- 给 `messages` 加 `source` 列（如 `text` / `voice` / `sticker`）：语义最干净，但引入迁移、动到共享的 `db/` 与所有消息读取点，改动面远超本特性所需。记录为**未来的可选演进**，不在本次范围。
- 不标记来源：满足不了 FR-009，否决。

**已知取舍**：前缀会随内容进入上下文，理论上也可能进记忆沉淀。这属于可接受的噪音（与既有贴纸占位描述 `[对方发来一张表情包 …]` 完全同源的既有做法）。

---

## D5. 听不清的处理：交给模型，不写死话术（FR-005）

**Decision**: 转写为空 / 无语音 / 解码失败等"内容层面"失败，落一条**说话人视角的标记文本**，然后**照常起轮次**——让模型结合上下文自己说"刚没听清，你再说一遍？"。

**Rationale**:
- 用户明确要求：像两个真人一样，听不清的一方开口让对方重说，**具体回什么由模型生成**。
- 标记文本是"模型能读懂的事实"，不是面向用户的成品话术——所以"仓库里不内置固定话术"这条约束仍成立（仓库提供的是**情境**，措辞由模型给）。
- 起轮次而非特判，复用了既有的插话 / 重试 / 落库 / 记忆链路，没有新的分支通道。

**Alternatives considered**:
- 通道层直接回一句固定文案：违反 FR-005 与用户原话，否决。
- 完全不起轮次、静默丢弃：就是当前 bug 的行为，违反 FR-001/SC-002，否决。

---

## D6. 能力不可用的处理：系统故障路径（FR-006/013）

**Decision**: 网关未启用 / 不可达 / 返回 5xx / 引擎不可导入 → **不落库、不起轮次**，`tracing::error!` 记录（含 `chat_id`、失败原因），同时启动时对 `/health` 探测并在日志里给出能力状态。

**Rationale**:
- 用户明确要求：这是程序错误的一部分，待开发者修缺陷；不得用面向用户的话术掩盖。
- 不落库是关键：若为它写一条"没听清"的占位消息，就把**缺陷伪装成了内容问题**，用户会以为是自己的语音有问题。不落库让时间线保持诚实，错误只出现在日志里。
- 启动探测解决"随机不可用"——能力状态在进程启动时就明确可见（FR-013）。

**Alternatives considered**:
- 退化为"听不清"（落占位消息让模型回应）：掩盖缺陷，且/或让用户以为要重发语音——否决。
- 让用户看到一句"当前不支持语音"：既违反 FR-006（能力不可用不作对话反馈），也会让一个**可修复的缺陷**变成永久的用户可见降级——否决。

---

## D7. 错误分界线：HTTP 状态码即语义

**Decision**: 网关的 `/transcribe` 用状态码区分两类失败，Rust 严格按此分流：

| 状态码 | 语义 | Rust 归类 |
|---|---|---|
| `200` | 成功（`text` 可为空串 = 没听出内容） | 成功 / 听不清（看 `text` 是否为空） |
| `400` / `413` / `415` / `422` | **这条音频的问题**（解码失败 / 超限 / 格式不支持 / 无语音） | 内容层面 → 交模型自然回应 |
| `503` | **引擎不可用**（未安装 / 加载失败） | 系统故障 |
| `500` | 内部错误 | 系统故障 |
| 连接失败 / 超时 | 网关没起或卡住 | 系统故障 |

**Rationale**: 光靠错误文本字符串匹配太脆；用状态码这一类"结构化契约"做分流，既稳定又可单测（`telegram_tests.rs` 里直接喂状态码断言归类）。契约细节见 `contracts/transcribe-api.md`。

**Alternatives considered**: 响应体里加 `kind` 字段（`unintelligible` / `engine_unavailable`）并只靠它分流。最终**两者都要**：状态码是主判据，`kind` 作为补充信息写进日志，便于排查。

---

## D8. 时序：新鲜度分级前移，过期语音不转写

**Decision**: 在 `run_loop` 里把 `classify_inbound` 移到入站内容解析**之前**；判定为 `TooOld` 的语音落一条过期占位文本并 `continue`，**不调用转写**。

**Rationale**:
- 现状 `inbound_text()` 在分级前调用是安全的（纯函数、零成本）。语音解析要联网、要跑模型，成本与延迟都实打实，因此必须在确认"这条消息值得回复"之后再付这个成本。
- 文本/贴纸路径的最终行为不变（`TooOld` 本来就只入库不回复），只是判定顺序调整。

**Alternatives considered**: 保持原顺序、对所有语音一律转写。会为进程重启后补拉的成批历史语音白跑一遍模型，否决。

---

## D9. 音频不手工转码

**Decision**: 直接把 Telegram 给的原始字节（`voice` 通常是 OGG/OPUS）以 `Content-Type: audio/ogg`（取报文里的 `mime_type`，缺失则按 `voice`/`audio` 分别回退 `audio/ogg` / `audio/mpeg`）POST 给网关。

**Rationale**:
- `faster-whisper` 通过 PyAV 解码，OGG/OPUS 原生支持——**Spike 实测已确认**：直接把 SAPI 转出的 OGG/OPUS 字节喂进去，无需任何预处理即转出正确文本。
- 网关侧若要 ffmpeg 转码，等于把网关的**离线**属性绑到第二个外部二进制上（现有 SAPI 引擎已依赖 ffmpeg，但 ASR 不该再叠一层）。
- 解码失败不是缺陷而是"这条音频的问题" → 走 4xx → 交给模型，与 D5 一致。

**必须同时满足的两个前提**（否则 D9 不成立）：
- PyAV 版本 MUST 满足 `av<19`（见 D1 依赖锁定）——否则解码路径在拿到字节之前就 `TypeError` 崩了。
- `transcribe()` MUST 开启 `vad_filter=True`（见 D1）——否则"解码成功但无人声"会被输出成幻觉文本。

**Alternatives considered**: Rust 侧先 ffmpeg 转 16 kHz WAV 再上传。多一次磁盘/进程往返，且要求 `sa-link` 依赖外部二进制，否决。

---

## D10. 上限与超时（FR-003）

**Decision**（三层，缺一不可）：
- **体积硬上限**：`file_size` > 20 MB（Telegram Bot API `getFile` 的下载上限）→ **不下载**，判为内容层面失败。
- **时长前置上限**：`duration` > `VOICE_TRANSCRIBE_MAX_SECS`（默认 **120s**）→ **不下载、不转写**，直接判为内容层面失败并落"内容太大/太长没能听清"标记。
- **转写超时**：`VOICE_TRANSCRIBE_TIMEOUT`（默认 **180s**）→ 归入系统故障路径（D6）。

**Rationale**:
- 时长前置上限是**为修 FR-003 的矛盾而加**：原先只靠超时兜底，一条 3 分钟语音会一路跑到超时（120s），表现为"用户长时间收不到任何东西 + 最终静默"——**恰好是 FR-003 明令禁止的"长时间无响应"**。前置拦截让超时重新变成"确实是异常"的信号。
- 三个数必须**成对配置**，否则会自相矛盾。Spike 实测 `small` 在 CPU 上的 RTF = **0.69x ~ 1.00x**（多次实测；真实 HTTP 路径下也测到 1.00x），按**最坏值 1.0x** 取值：
  - 时长上限 120s → 最坏约 **120s** 转写
  - 再加最坏的**冷加载 46s**（预热失败或未开预热时才会落在请求路径上）→ **166s**
  - 超时 180s > 166s ✅ 默认值在实测最坏情况下**仍然成立**，但余量只有 14s
  - 结论：`VOICE_TRANSCRIBE_MAX_SECS` 若要调大，`VOICE_TRANSCRIBE_TIMEOUT` 必须按 `MAX_SECS × 1.0 + 60` 给足；反之把 `ASR_WARMUP` 关掉会吃掉这 46s 余量，推荐保持开启
- 20 MB 对 OPUS 是极宽裕的余量（实测 2.68s 语音仅 9.3 KB），它只用来挡"用文件方式发的超长音频"。
- 超时必须有硬边界，否则会拖住长轮询循环（Technical Context 已记录该约束）。

**Alternatives considered**:
- **按时长拦截但让超时兜底**（= 本特性原先的设计）：已在 analyze 中被判为 HIGH 冲突，否决。
- **把转写超时改判为内容层面失败**：也能自洽，但会让"网关卡死/模型加载失败"这类真故障伪装成"你的语音太长"——违反 D6 的取向。故保留超时=系统故障，另加时长前置拦截。
- 只按体积拦截：OPUS 压缩率高，20 MB 可容纳极长音频，拦不住"长但不大"的语音条。故体积与时长**都要**。

---

## 汇总：本特性不引入的东西（避免过度设计）

- ❌ 新 Rust crate、新依赖层
- ❌ 数据库 schema 变更 / 迁移 / 新表
- ❌ 对 `src/api/**`、`src/channels/ilink/**`、前端 的任何改动
- ❌ 仓库内内置"没听清"固定话术（改由模型生成）
- ❌ 面向用户的"当前不支持语音"降级文案（能力不可用只走错误路径）

## 汇总：本特性引入的东西（供 review 用）

- **Rust 侧新增 3 个环境变量**：`VOICE_TRANSCRIBE_ENABLED`（默认 `false`）、`VOICE_TRANSCRIBE_TIMEOUT`（默认 `180`）、`VOICE_TRANSCRIBE_MAX_SECS`（默认 `120`）——同步到 `backend/.env.example`。
- **网关侧新增 7 个环境变量**：`ASR_ENGINE` / `ASR_MODEL` / `ASR_LANGUAGE` / `ASR_DEVICE` / `ASR_COMPUTE_TYPE` / `ASR_MODEL_DIR` / `ASR_WARMUP`——同步到 `media-gateway/.env.example`。
- **1 个新 HTTP 端点**：网关 `POST /transcribe`（+ `/health` 增 `asr` 段）。
- **1 条安装约束**：`av<19`（否则装上即崩）。
- **1 条运行不变量**：转写必须开 `vad_filter=True`（否则静音幻觉成文本）。
- **2 条内容约定（不落列）**：`[语音] ` 前缀表示来源；失败用说话人视角的标记文本，措辞由模型定。
