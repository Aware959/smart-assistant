# 媒体网关（media-gateway）

让 `smart-assistant` 调用你本机软件生成图片 / 语音的独立服务。Rust 后端只通过 HTTP 调它，
不关心背后是哪个扩散软件或 TTS 库——这一层专门负责"调起本机 AI 软件并把字节交给 Telegram"。

## 架构位置

```
模型 → generate_image / generate_voice 工具（Rust 内核）
  → 异步投递工人 POST http://127.0.0.1:8787/generate/{image,voice}
  → 本服务：调扩散软件 / TTS 出字节
  → 返回图片/音频字节
  → Rust 上传到 Telegram（sendPhoto / sendVoice）
```

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

## 健康检查

```powershell
curl http://127.0.0.1:8787/health
```

## 排错

- **启动即报"启动失败：无法绑定 127.0.0.1:8787"**：端口已被占用（多半是另有一个
  `python main.py` 还在跑）。本服务不抢占端口、直接退出，先关掉旧进程再启动。
- **连接被拒 / 502**：扩散后端没起，或没开 `--api`；确认 `DIFFUSION_BACKEND_URL` 能单独 `curl` 通。
- **Rust 侧报连接错**：本机请求被 Clash 系统代理劫持。Rust 侧已对网关 client 强制 `.no_proxy()`，
  正常不会发生；若仍报错请检查该处是否被改。切勿在 `backend/.env` 设 `NO_PROXY=*`，
  那会连 Telegram 轮询依赖的代理一起绕过。
- **语音 502/500**：sapi 引擎报 500 多半是找不到 `ffmpeg`（装好并进 `PATH`，或用 `FFMPEG_BIN`
  指定绝对路径）；edge-tts 引擎需联网，且要能 `import edge_tts`（老 Python 更容易装）。
