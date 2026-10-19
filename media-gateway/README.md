# 媒体网关（media-gateway）

让 `smart-assistant` 调用你本机软件生成图片 / 语音的独立服务。Rust 后端只通过 HTTP 调它，
不关心背后是哪个扩散软件或 TTS 库——这一层专门负责"调起本机 AI 软件并把字节交给 Telegram"。

## 架构位置

```
媒体（出站，模型决定要发东西）：
模型 → generate_image / generate_voice 工具（Rust 内核）
  → 异步投递工人 POST http://127.0.0.1:8787/generate/{image,voice}
  → 本服务：调扩散软件 / TTS 出字节
  → 返回图片/音频字节
  → Rust 上传到 Telegram（sendPhoto / sendVoice）

语音转写（入站，用户发来语音）：
用户语音条 → Rust 的 Telegram 通道下载裸字节 → POST http://127.0.0.1:8787/transcribe
  → 本服务：faster-whisper 转文字（可选依赖，见下）
  → 返回 {"text","language","duration"}
  → Rust 把文本当用户消息落库并起轮次（用户看不到转写文字）
```

出入站是**两件独立的事**：出站能力由 `MEDIA_GATEWAY_ENABLED` 控制，入站转写由
`VOICE_TRANSCRIBE_ENABLED` 控制，互不影响。

## 代码结构

纯标准库实现，按职责拆分为 `gateway/` 包（依赖单向向下）：

```
media-gateway/
├── main.py                 入口：加载配置并启动 HTTP 服务
├── gateway/
│   ├── config.py           .env 加载 + 环境变量 → Settings（不可变配置）
│   ├── errors.py           GatewayError（带 HTTP 状态码 + 回显文本）
│   ├── httpclient.py       urllib 助手（自动读 NO_PROXY，绕过本机代理）
│   ├── imaging/            出图
│   │   ├── __init__.py       generate_image()：按 DIFFUSION_BACKEND_TYPE 分派
│   │   ├── a1111.py          A1111 后端
│   │   ├── comfyui.py        ComfyUI 后端（提交 /prompt → 轮询 /history → /view）
│   │   └── workflow.py       工作流载入 + 节点自动识别（find_node / resolve_nodes）
│   ├── speech/             出声
│   │   ├── __init__.py       generate_voice()：按 TTS_ENGINE 分派
│   │   ├── sapi.py           Windows SAPI + ffmpeg（离线）
│   │   └── edge.py           edge-tts（在线，延迟导入）
│   ├── asr/                听写（可选依赖，见「语音识别」一节）
│   │   ├── __init__.py       transcribe()：按 ASR_ENGINE 分派 + 可导入性探测 + 错误映射
│   │   └── faster_whisper.py faster-whisper 适配（懒加载 + 进程内模型单例 + warmup）
│   └── server.py           http.server 路由与启动
├── tests/                  unittest（零依赖）
├── .env / .env.example     配置
├── SD模板2.json            ComfyUI API 格式工作流（由 COMFYUI_WORKFLOW_FILE 指向）
└── README.md
```

**扩展点**：加一个扩散后端 → 在 `gateway/imaging/` 加文件实现 `generate(req) -> (bytes, mime)`，
再在 `gateway/imaging/__init__.py` 的分派里挂一行、`config.py` 加所需字段即可；加 TTS 引擎同理走
`gateway/speech/`。

**测试**（在 `media-gateway/` 下运行）：

```powershell
python -m unittest discover -v
```

单测本身零依赖（ASR 用假引擎注入，不要求真的装 `faster-whisper`），任何解释器都能跑；想顺带验证
真引擎，换成 `media-gateway/.venv/Scripts/python.exe -m unittest discover -v` 即可。

## 启动

```powershell
cd media-gateway
cp .env.example .env       # 按需改 .env（端口 / 后端类型 / ComfyUI 工作流 / TTS 引擎等）
python main.py              # 监听 127.0.0.1:8787
```

退出（对齐 Rust 侧 `sa-link`）：**Ctrl+C / Ctrl+Break** 走优雅收尾——停止接收新请求，
并等**在途**的出图 / 合成请求跑完再退出（避免把正在传输的图片/语音掐断）；**再按一次**
不再等待、立即强制退出。

端口占用：本服务**不设** `SO_REUSEADDR`（Windows 上该选项会让第二个网关「抢绑」同一端口、
静默并存），因此端口被占时会**直接报错退出**（退出码 1），不会出现两个网关同时监听。
实测「退出后立即重启」不受影响。

图片生成零依赖（纯标准库）。语音默认走 Windows SAPI + **ffmpeg**（离线、零 Python 依赖），
只需 `ffmpeg` 在 `PATH` 里；只有当你把 `TTS_ENGINE` 切成 `edge-tts` 时才需要 `pip install edge-tts`。

配置从同目录的 `.env` 读取（也可在终端 export 覆盖，终端变量优先于 `.env`）。
完整变量见 `.env.example`。

> 网关只用 Python 标准库实现（http.server + urllib），不依赖 fastapi/uvicorn/httpx，
> 因此在较新的 Python（如 3.15，anyio 等异步依赖不兼容）上也能直接跑。

后端 `backend/.env` 需打开开关：

```
MEDIA_GATEWAY_ENABLED=1
MEDIA_GATEWAY_URL=http://127.0.0.1:8787
```

> 不要在 `backend/.env` 里设 `NO_PROXY=*`。Rust 侧已对网关 client 强制 `.no_proxy()`，
> 无需全局设置；而 `NO_PROXY=*` 会让 reqwest 连 Telegram 轮询依赖的 Clash 系统代理一并
> 绕过，表现为 bot 完全收不到消息（本仓库踩过这个坑）。

## 图片（扩散软件）

支持 **A1111** 与 **ComfyUI** 两种后端，由 `DIFFUSION_BACKEND_TYPE` 选择。

### A1111

默认后端（StabilityMatrix 前端最常见）。需在 StabilityMatrix 启动参数里勾上 `--api`
（开放 `/sdapi/v1/txt2img`），否则本服务连不上。

### ComfyUI（你的后端）

ComfyUI 没有同步出图接口：本服务把工作流 POST 到 `/prompt` 拿到 `prompt_id`，轮询
`/history` 直到完成，再从 outputs 取第一张图经 `/view` 拉回字节。

最稳的用法是导出**你自己的**工作流（ComfyUI 菜单 "Save (API Format)"，里面已接好你的
checkpoint / 模型），只需通过 `COMFYUI_WORKFLOW_FILE` 告诉本服务文件路径即可——**节点 id
会自动识别**，不需要你手动填。

**节点自动识别规则**（`gateway/imaging/workflow.py` 的 `resolve_nodes`）：

- 顺着 `KSampler` 节点的 `positive` / `negative` 输入，定位到正向 / 负向提示词所在的
  `CLIPTextEncode` 节点；
- 按 `class_type == "EmptyLatentImage"` 自动找到尺寸节点；
- 环境变量的 `*_NODE` 仅作**可选覆盖**：只有当指定的 id 真实存在于图中时才生效，否则改走自动识别。

即你只需两步：① 在 ComfyUI 里 "Save (API Format)" 导出工作流；② 在 `.env` 里设
`COMFYUI_WORKFLOW_FILE=/路径/你的工作流.json`。其余全交给自动识别。

| 环境变量 | 默认 | 说明 |
| --- | --- | --- |
| `DIFFUSION_BACKEND_URL` | `http://127.0.0.1:8188` | 后端地址（看 ComfyUI 监听端口） |
| `DIFFUSION_BACKEND_TYPE` | `a1111` | 设为 `comfyui` 启用 ComfyUI 路径 |
| `COMFYUI_WORKFLOW_FILE` | 空（用内置默认） | 你导出的 API 格式工作流 JSON 路径（推荐设置；相对路径按 `media-gateway/` 解析） |
| `COMFYUI_PROMPT_NODE` | `6` | 可选覆盖：正向提示词节点 id（仅在图中存在时生效） |
| `COMFYUI_NEGATIVE_NODE` | `7` | 可选覆盖：负向提示词节点 id（仅在图中存在时生效） |
| `COMFYUI_SIZE_NODE` | `5` | 可选覆盖：EmptyLatentImage 节点 id（仅在图中存在时生效） |
| `COMFYUI_KSAMPLER_NODE` | `3` | 可选覆盖：KSampler 节点 id（仅在图中存在时生效） |

> 不指定 `COMFYUI_WORKFLOW_FILE` 时用内置最简工作流兜底：它会先调 `/object_info` 自动挑一个
> 你机器上真实存在的 checkpoint 填进 `CheckpointLoaderSimple`（硬编码 ckpt 名多半会 400），
> 并带 `filename_prefix: smart-assistant` 以符合新版本 ComfyUI 对 SaveImage 的必填项要求。
> **正式用请导出你自己的工作流**并指向 `COMFYUI_WORKFLOW_FILE`，无需关心内部节点 id。

请求体（均为可选，缺省有合理默认；尺寸/步数/CFG 仅当对应节点存在时才覆盖）：

```json
{ "prompt": "a cat on a windowsill at sunset", "negative_prompt": "", "width": 512, "height": 512, "steps": 20, "cfg_scale": 7 }
```

## 语音（TTS）

两种引擎，由 `TTS_ENGINE` 选择，都产出 Telegram `sendVoice` 能直接吃的格式：

### sapi（默认，离线）

用 Windows 自带的 **SAPI** 合成 WAV，再用 **ffmpeg** 转成 `OGG/OPUS`（Telegram 原生语音
气泡）。零 Python 依赖、不联网，长这样跑：

- 前置：`ffmpeg` 在 `PATH` 里（或用 `FFMPEG_BIN` 给绝对路径）；
- 音色：`TTS_VOICE` / `SAPI_VOICE` 填 **SAPI 音色全名**，如 `Microsoft Huihui Desktop`
  （中文女声）、`Microsoft Kangkang`（中文男声）。留空或填了本机没有的名字时，会自动退到
  已安装的 `zh-*` 音色（避免拿英文音色念中文）；
- 语速：请求里的 `rate`（如 `+20%`）会折算到 SAPI 的 -10..10 档。

### edge-tts（联网，可选）

微软在线音色，质量更好，但依赖 `aiohttp`——在较新的 Python（如 3.15）上可能装不上/导入即
崩，因此**不是默认**。要用得先 `pip install edge-tts`，并把 `TTS_ENGINE` 设为 `edge-tts`。

| 环境变量 | 默认 | 说明 |
| --- | --- | --- |
| `TTS_ENGINE` | `sapi` | `sapi`（Windows 离线）\| `edge-tts`（联网） |
| `TTS_VOICE` | `zh-CN-XiaoxiaoNeural` | 音色名（sapi 用 SAPI 全名；edge-tts 用其音色名） |
| `SAPI_VOICE` | 空 | 可选：显式指定 SAPI 音色全名，优先级高于 `TTS_VOICE` |
| `FFMPEG_BIN` | `ffmpeg` | sapi 引擎转码用的 ffmpeg 可执行文件 |

请求体（`voice` / `rate` 可选，缺省用上面的配置）：

```json
{ "text": "晚安，早点睡", "voice": "Microsoft Huihui Desktop", "rate": "+0%" }
```

## 语音识别（ASR，入站）

把用户发来的语音条转成文字。**这与上面出站的 TTS 是两件独立的事**：出站由 `generate_voice`
走 SAPI / edge-tts，入站转写走 `faster-whisper`，由不同开关控制、可各自单独使用。

前端是 Rust 的 Telegram 通道：它从 Telegram 下载语音裸字节，`POST /transcribe`（body 就是原始
音频字节，`Content-Type` 带音频 MIME），拿回 JSON：

```powershell
curl.exe -X POST --data-binary "@voice.ogg" -H "Content-Type: audio/ogg" http://127.0.0.1:8787/transcribe
# → {"text":"今天晚上想吃什么?","language":"zh","duration":2.68}
```

| 响应字段 | 说明 |
| --- | --- |
| `text` | 转写文本。**空串表示"没听到人声"**（静音已由 VAD 滤掉），不是报错 |
| `language` | 识别到的语言，如 `zh` |
| `duration` | 音频时长（秒），可为 `null` |

失败按 HTTP 状态码区分：`400` 音频无法解码 · `413` 音频过长 · `415` 格式不支持 ·
`422` 无有效语音 · `503` 引擎未安装 / 加载失败（`kind="engine_unavailable"`）·
`500` 其他内部错误。Rust 侧按这个分类决定"当没听清"还是"记故障日志"。

### 安装前提

**不装也能跑**：没装 `faster-whisper` 时网关其余功能完全不受影响，只是 `/transcribe` 返回 503、
`/health` 的 `asr.importable` 为 `false`。

`faster-whisper` 依赖 `av` 与 `ctranslate2`，**建议单独建 venv**（仓库已把 `media-gateway/.venv/`
加进 `.gitignore`）：

```powershell
cd media-gateway
uv venv --python 3.13                      # 用 uv 装一个独立的 CPython 3.13
.venv\Scripts\python.exe -m pip install faster-whisper "av<19"
```

> **两条硬约束**（都是实机踩出来的）：
> - **解释器用 `uv venv --python 3.13`（或 `uv python install 3.13`）**，别用系统里较新的
>   Python（如 3.15，`ctranslate2` / `av` 的轮子还没跟上），也别用 `py -3.13`——本机不可用。
> - **必须锁 `av<19`**：`av 19.x` 移除了 `av.open(metadata_errors=…)`，与 faster-whisper 1.2.1
>   不兼容，装上即崩。

首次调用会从 HuggingFace 拉模型（默认 `small`，约 500 MB）。网络不通时用镜像兜底：

```powershell
$env:HF_ENDPOINT = "https://hf-mirror.com"
```

想固定模型缓存目录就设 `ASR_MODEL_DIR`。

### 环境变量

| 环境变量 | 默认 | 说明 |
| --- | --- | --- |
| `ASR_ENGINE` | `faster-whisper` | 转写引擎；留空或 `none` 表示本网关**不提供**转写（`/transcribe` 返回 503） |
| `ASR_MODEL` | `small` | 模型名（`tiny` / `base` / `small` / `medium` / `large-v3` 等） |
| `ASR_LANGUAGE` | `auto` | 识别语言；`auto` 自动判定，也可写死 `zh` |
| `ASR_DEVICE` | `cpu` | 推理设备（`cpu` / `cuda`） |
| `ASR_COMPUTE_TYPE` | `int8` | 量化类型（CPU 常用 `int8`，GPU 常用 `float16`） |
| `ASR_MODEL_DIR` | 空 | 模型缓存目录；留空用默认的 HuggingFace 缓存 |
| `ASR_WARMUP` | `1` | 启动时后台预热（见下） |

### 冷启动预热与 `ASR_WARMUP`

模型加载很慢（实测冷加载最坏约 46 秒）。`ASR_WARMUP=1`（默认）时，网关启动后**用后台线程**把
模型预先加载好，监听启动不受阻塞；`/health` 的 `asr.model_loaded` 就是这个状态的可观测入口——
预热期间 `/health` 仍能秒回，只是 `model_loaded` 还是 `false`。

关掉 `ASR_WARMUP` 的话，这 46 秒会转嫁到"第一个发语音的人"身上，容易撞上 Rust 侧的转写超时。

关于 `vad_filter`：引擎**强制开启**（`gateway/asr/__init__.py` 的 `FORCED_VAD_FILTER`，不能从请求里
关掉）。不开 VAD 时，纯静音会被模型幻觉成 `'You'` 之类的文本，把"没听清"误当成有效发言——
这是正确性问题，不是体验问题。

## 健康检查

```powershell
curl http://127.0.0.1:8787/health
```

入站转写能力看 `asr` 段：

```json
{ "asr": { "engine": "faster-whisper", "importable": true, "model_loaded": true } }
```

`importable: false` = 依赖没装（或 `ASR_ENGINE=none`），此时 `/transcribe` 一律 503；
`model_loaded: false` = 模型还没加载好（预热中，或首次调用前）。

## 排错

- **启动即报"启动失败：无法绑定 127.0.0.1:8787"**：端口已被占用（多半是另有一个
  `python main.py` 还在跑）。本服务不抢占端口、直接退出，先关掉旧进程再启动。
- **连接被拒 / 502**：扩散后端没起，或没开 `--api`；确认 `DIFFUSION_BACKEND_URL` 能单独 `curl` 通。
- **Rust 侧报连接错**：本机请求被 Clash 系统代理劫持。Rust 侧已对网关 client 强制 `.no_proxy()`，
  正常不会发生；若仍报错请检查该处是否被改。切勿在 `backend/.env` 设 `NO_PROXY=*`，
  那会连 Telegram 轮询依赖的代理一起绕过。
- **语音 502/500**：sapi 引擎报 500 多半是找不到 `ffmpeg`（装好并进 `PATH`，或用 `FFMPEG_BIN`
  指定绝对路径）；edge-tts 引擎需联网，且要能 `import edge_tts`（老 Python 更容易装）。
- **`/transcribe` 一直 503**：`faster-whisper` 没装进**正在跑的那个解释器**（先看 `/health` 的
  `asr.importable`），或 `ASR_ENGINE` 被设成了空 / `none`。
- **第一条语音特别慢 / 转写超时**：模型冷加载（最坏约 46 秒）。保持 `ASR_WARMUP=1`，或提前手动
  打一次 `/transcribe` 把模型热起来。
- **静音/杂音返回 `text` 为空**：正常。VAD 把没有人声的段落滤掉了，Rust 侧据此判"没听清"并让
  模型自然回应。
