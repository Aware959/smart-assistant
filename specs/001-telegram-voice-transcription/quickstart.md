# Quickstart: 验证 Telegram 语音消息识别

本文件是**验证/运行指南**，用于端到端证明本特性可用。接口字段不在此重复，见 [contracts/transcribe-api.md](./contracts/transcribe-api.md)；内容约定见 [data-model.md](./data-model.md)。

> ⚠️ 宪法原则 IV：以下所有验证 MUST 让 `SMART_ASSISTANT_DB` 指向**临时库**，MUST NOT 用默认的 `smart_assistant.db`。验证结束 MUST 按最后一节清理。

---

## 0. 前置条件

| 项 | 要求 | 检查命令 |
|---|---|---|
| Python（网关） | **uv 安装的 CPython 3.13**（ASR 需要；见 research.md D2） | `uv python list --only-installed` |
| 系统默认 `python` | 是 3.15.0a8，**不能**装 faster-whisper（无 cp315 wheel） | `python -V` |
| `py -3.13` | **本机不可用**（注册项过期 + shim 断链），别用它 | `py -3.13 -V` → 报 `No suitable Python runtime found` |
| Telegram | 有效的 `TELEGRAM_BOT_TOKEN`，且你的 `chat_id` 在 `TELEGRAM_ALLOWED_IDS` 内 | 查 `backend/.env` |
| 网络 | 首次拉模型需访问 HuggingFace（本机实测直连可用；不通时设 `HF_ENDPOINT=https://hf-mirror.com`） | — |

> 以上三行是 Spike 在**本机实测**的结论，不是推测。换机器请重新核一遍解释器与依赖。

## 1. 装 ASR 依赖（用 uv 装 3.13，不动系统默认解释器）

```powershell
uv python install 3.13                                     # 实测装得 cpython-3.13.16，独立于系统
cd media-gateway
uv venv --python 3.13 .venv
uv pip install --python .venv\Scripts\python.exe faster-whisper "av<19"
```

> **`"av<19"` 不能省**：`av 19.x` 删除了 `av.open(metadata_errors=...)`，而 faster-whisper 1.2.1 仍在传这个参数 → 装上即崩（`TypeError`）。Spike 实测 `av 18.1.0` 正常。

验证装对了（应能打印版本，且**不触发任何源码编译**）：

```powershell
.\.venv\Scripts\python.exe -c "import av, ctranslate2, faster_whisper; print(av.__version__, ctranslate2.__version__, faster_whisper.__version__)"
```

期望输出形如：`18.1.0 4.8.3 1.2.1`

**冒烟测试**（可选但强烈建议，能一次排除 90% 的环境问题）：录一段中文语音存成 `test.ogg`，然后

```powershell
.\.venv\Scripts\python.exe -c "from faster_whisper import WhisperModel; m=WhisperModel('small',device='cpu',compute_type='int8'); print(''.join(s.text for s in m.transcribe(r'test.ogg', vad_filter=True)[0]))"
```

Spike 实测：2.68 秒中文语音 → 正确文本，转写耗时 1.84s（多次实测 2.6~2.7s，随 CPU 负载波动，RTF 0.69x~1.00x）；模型首次下载 `small` 约 484 MB，冷加载 21~46s。真实 HTTP 路径亦已复验：`POST /transcribe` 送 9293 字节 OGG/OPUS → `200 {"text":"今天晚上想吃什么?","language":"zh"}`；静音 → `200 {"text":""}`（VAD 生效）；坏字节 → `400`。

## 2. 配置

`media-gateway/.env`：

```
ASR_ENGINE=faster-whisper
ASR_MODEL=small
ASR_LANGUAGE=auto
ASR_DEVICE=cpu
ASR_COMPUTE_TYPE=int8
ASR_WARMUP=true
```

`backend/.env`（注意：**新增临时库路径**）：

```
SMART_ASSISTANT_DB=smart_assistant_voice_test.db
MEDIA_GATEWAY_URL=http://127.0.0.1:8787
VOICE_TRANSCRIBE_ENABLED=1
VOICE_TRANSCRIBE_TIMEOUT=180
VOICE_TRANSCRIBE_MAX_SECS=120
```

> `MEDIA_GATEWAY_ENABLED` 可以不置位——它与入站转写无关（它管的是出图/出语音工具的下发）。
>
> `TIMEOUT` 必须大于 `MAX_SECS × 1.0 + 60`（Spike 实测 CPU RTF 0.69x~0.98x，按最坏 1.0x 算，另加最坏冷加载 46s）。默认 120 + 46 = 166s < 180s ✅。改大 `MAX_SECS` 时要同步改大 `TIMEOUT`，否则合法语音会撞超时并被误判成系统故障；也**别关 `ASR_WARMUP`**，那 46s 余量就是它挣来的。

## 3. 起两个进程

```powershell
# 终端 A：网关（用 venv 里的 3.13 解释器）
cd media-gateway
.\.venv\Scripts\python.exe main.py

# 终端 B：通道守护
cd backend
cargo run --bin sa-link
```

## 4. 能力可用性自检（FR-013，先验这条）

```powershell
curl http://127.0.0.1:8787/health
```

**期望**：JSON 含 `"asr": {"engine": "faster-whisper", "importable": true, "model_loaded": ...}`；`ASR_WARMUP=true` 时预热完成（`small` 实测 21.4s）后 `model_loaded` 变 `true`。

**期望**：`sa-link` 启动日志里出现一行明确的语音转写能力状态（已启用且可导入 / 未启用 / 不可导入）。反向验证：把 `VOICE_TRANSCRIBE_ENABLED` 置 `0` 重启，日志应说明"未启用"，而不是沉默。

## 5. 验证场景

### 场景 A —— 说一句就能聊（P1 / FR-001/002/010）

1. 在 Telegram 里发一条 5 秒左右、内容清晰的语音，例如："今天晚上想吃什么"。
2. **期望**：助手给出与语音内容相符的回复；**用户不会看到转写文字**（不回显，FR-010）。
3. **期望**：临时库里该条消息 `content` 形如 `[语音] 今天晚上想吃什么`（来源可辨，FR-009）。
4. 接着发一条文字延续同一话题。
   **期望**：助手的回复体现它把前面那条语音当上下文（话题连续）。

### 场景 B —— 听不清由模型自然回应（P2 / FR-004/005）

1. 发一条**没有人声的音频**（例如录一段静音/环境噪音）。
2. **期望**：助手回一句自然的话，表达没听清/没明白（可能邀请你重说或改用文字）。
3. 再发一次同类音频。
   **期望**：措辞**不完全相同**——证明是模型生成的，不是仓库里的固定模板。
4. **期望**：临时库里该条消息 `content` 为 `[对方发来一条语音，但没能听清内容]`。

### 场景 C —— 能力不可用走系统故障路径（P2 / FR-006/013）

1. **关掉网关**（终端 A `Ctrl+C`）。
2. 发一条语音。
3. **期望**：**没有任何回复，也没有任何"没听清"之类的话术**。
4. **期望**：`sa-link` 日志里出现 `error` 级记录，含 `chat_id` 与失败原因（连接失败）。
5. **期望**：临时库里**没有**新增该条消息（不落库 → 时间线不撒谎）。
6. 重启网关，再发一条语音。
   **期望**：恢复正常（瞬时故障不残留）。

### 场景 D —— 连续说也不掉链子（P3 / FR-007）

1. 快速连发两条内容相关的语音（如"我明天要出差" + "大概去三天"）。
2. **期望**：助手在一次回复里把两条都考虑到，无漏答。
3. **期望**：临时库里两条都按发送顺序存在。

### 场景 E —— caption 并存（FR-012）

1. 以**文件**方式发一段带文字说明的音频（caption 写"这段是昨天会议的录音"）。
2. **期望**：落库内容为 `"这段是昨天会议的录音\n[语音] <转写文本>"`——caption 没有被转写结果覆盖。

### 场景 F —— 超限（FR-003）

1. 发一条**超过时长上限**的语音（长于 `VOICE_TRANSCRIBE_MAX_SECS`，默认 120 秒）。
2. **期望**：助手**很快**（不下载、不转写）自然告知无法处理这条，落库 `[对方发来一条语音，但内容太大没能听清]` 走模型；**不是长时间无响应，也不是静默丢弃**。
3. 再发一个超过 20 MB 的音频文件（Telegram 允许发送，Bot API 不允许 bot 下载）。
4. **期望**：同上，走体积拦截。

> 这一步是 H1 修复的**验收点**：修复前，长语音会一路跑到转写超时（数分钟）才静默失败，正是 FR-003 禁止的"长时间无响应"。

### 场景 G —— 计时（SC-001）

1. 准备一句固定文本，例如"今天晚上想吃什么"。
2. 先**发文字**，从发送到收到回复计时，记 `T_text`。
3. 再**发同一句的语音**，同样计时，记 `T_voice`。
4. **期望**：`T_voice ≤ 2 × T_text`。
5. 在下方表格留痕（这是 SC-001 的唯一验收依据）：

| 轮次 | T_text (s) | T_voice (s) | 2×T_text | 达标 |
|---|---|---|---|---|
| | | | | |

> 首次语音会额外等待模型加载（预热完成则不会）。若第一轮因冷启动超标，请先发一条无关语音热身，再正式计时，并在记录里注明。

### 场景 H —— 主题一致率抽样（SC-003）

1. 准备 20 句日常短句，覆盖不同话题（吃饭、出行、工作、闲聊……）。
2. 逐句**先发文字、再发语音**（同一句），记录两轮的助手回复。
3. 按 `spec.md` 中 SC-003 的**判定口径**逐组打勾：**核心意图（话题域）一致即算一致**，不要求措辞或信息量相同；话题偏离/答非所问/把语音理解成另一件事算不一致。
4. **期望**：一致数 ≥ 18 / 20。

| 组 | 句子 | 文字轮回复要点 | 语音轮回复要点 | 一致? |
|---|---|---|---|---|
| 1 | | | | |
| … | | | | |

**未达标时**：保留失败样本，并区分是 **ASR 误识别**（看临时库里 `[语音] …` 的落库文本对不对）还是**下游理解**问题——两者的修法完全不同。

---

## 6. 自动化测试

```powershell
# Rust：单测（含入站语音解析、分流判定、marker 生成）
cd backend
cargo test

# Rust：绿色基线完整性
cargo check --all-targets
cargo fmt --all -- --check

# Python：网关分派与错误映射（不需要真的装 faster-whisper）
cd media-gateway
.\.venv\Scripts\python.exe -m unittest discover -v
```

**注意**：`cargo test` 与上文手工验证互不干扰——单测全部走 `Database::in_memory()`，不碰任何磁盘库。

---

## 7. 验证收尾（必须做）

手工验证会在临时库里留下 `messages` / `memories` / `memory_vectors` / `sessions` 行（语音对话也会经 `settle_memory` 沉淀成记忆并进向量表）。

> 顺带是 **SC-002 的核对点**：每发一条语音，临时库**必有一行**新 `messages`；**或**（能力不可用路径）日志**必有** error 记录。两条都不成立，说明语音又被静默丢弃了。

1. 停掉 `sa-link` 与网关。
2. 删除临时库 `backend/smart_assistant_voice_test.db`（连同 `-wal` / `-shm`）。
3. 确认**真实库文件未被触碰**：`Get-Item backend/smart_assistant.db | Select LastWriteTime` 应保持验证前的时间戳。
4. 若曾误用默认库验证：清理时 `memories` 与 `memory_vectors` MUST **同步**按 `memory_id` 删除，并把删了什么告知用户。
5. 把 `backend/.env` 的 `SMART_ASSISTANT_DB` 改动还原。

---

## 8. 排错速查

| 现象 | 多半原因 |
|---|---|
| `py -3.13` 报 `No suitable Python runtime found` | 本机正常现象：`py` 里的 3.12/3.13 是过期注册项。改用 `uv python install 3.13` |
| 转写时报 `TypeError: open() got an unexpected keyword argument 'metadata_errors'` | `av` 版本过高。必须 `av<19`（`uv pip install "av<19"`） |
| `pip install faster-whisper` 开始编译 / 报 MSVC 错误 | 用错解释器了（用了 3.15）。确认是 `.venv\Scripts\python.exe` 且基于 3.13 |
| 静音/噪音被识别成" You""谢谢观看"之类 | 没开 VAD。必须 `vad_filter=True`（网关分派层强制） |
| 首次转写卡很久 | 正常：进程内首次调用在加载模型。开 `ASR_WARMUP=true` 可把这段挪到启动时 |
| 拉模型失败 | 网络到不了 HuggingFace；设 `HF_ENDPOINT=https://hf-mirror.com`，或用 `ASR_MODEL_DIR` 指向本地模型目录 |
| `/transcribe` 返回 503 | 引擎不可导入；用第 1 节的 `-c "import ..."` 复核 |
| `/generate/*` 也一起坏了 | 不应发生——ASR 是懒加载，失败必须收敛在 `/transcribe`。若出图/出语音也坏，说明改动越界了 |
| 长语音长时间没反应、最后什么都没有 | 检查 `VOICE_TRANSCRIBE_MAX_SECS` 前置拦截是否生效（H1 修复点） |
| 收到语音毫无反应且日志无 error | 检查 `VOICE_TRANSCRIBE_ENABLED` 是否置位，以及 `chat_id` 是否在白名单 |
