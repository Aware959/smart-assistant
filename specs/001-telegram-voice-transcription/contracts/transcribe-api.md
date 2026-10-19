# Contract: media-gateway `POST /transcribe`

本特性唯一新增的对外接口。契约的**双方**是：Rust 通道层（`backend/src/channels/telegram.rs`）与 Python 网关（`media-gateway/gateway/server.py`）。

设计依据：research.md D1（网关承载引擎）、D7（状态码即语义）、D9（不手工转码）、D10（上限与超时）。

---

## 请求

```
POST /transcribe
Content-Type: audio/ogg | audio/mpeg | audio/wav | ...   （必填）
Content-Length: <字节数>                                  （必填）
<body>: 原始音频字节（非 multipart、非 JSON、非 base64）
```

| 项 | 规则 |
|---|---|
| 方法 | 仅 `POST`；其他方法按网关既有约定返回 `404` |
| 传输格式 | **原始字节**。与 `/generate/*` 的 JSON 不同，此端点读 body 原文 |
| `Content-Type` | 由 Rust 从入站报文的 `mime_type` 透传；缺失时 `voice` → `audio/ogg`，`audio` → `audio/mpeg` |
| 体积 | 网关不设独立上限；上游（Rust）已在 **20 MB** 与 **`VOICE_TRANSCRIBE_MAX_SECS`** 两处拦截（前置，根本不会发过来）。网关侧的 `413` 只是**兜底**，正常路径不应出现 |
| 鉴权 | 无。网关只监听 `127.0.0.1`，与既有端点一致 |
| 代理 | Rust 侧对该客户端 MUST 显式 `.no_proxy()`（沿用 `media_generate()` 既有做法，否则本机 Clash 代理会劫持 `127.0.0.1`） |

## 响应

### 200 — 转写完成

```json
{
  "text": "今天晚上想吃什么",
  "language": "zh",
  "duration": 3.2
}
```

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `text` | string | ✅ | 转写结果。**允许为空串**——表示"音频里没听出内容"，属于正常结果而非错误 |
| `language` | string | ❌ | 识别语种，仅打日志 |
| `duration` | number | ❌ | 音频时长（秒），仅打日志 |

**实现约束（决定"空串"是否可信）**：网关 MUST 以 **`vad_filter=True`** 调用引擎。Spike 实测：静音不开 VAD 会返回 `'You'` 这类**幻觉文本**（`no_speech_prob=0.862`），此时"空串 ⇒ 没听清"的判据失效，幻觉内容会被当作用户发言进入对话——属正确性缺陷。

**调用方判定**：`text` 去空白后为空 → 归类 `Unintelligible`（交模型用自然语言回应）；非空 → 归类 `Transcribed`。注意：仅剩标点的结果视同**空**；但模型听出的简短有意义应答（如"嗯""好"）属**正常转写**，不得被判成"没听清"。

### 4xx — 这条音频的问题（内容层面）

```json
{ "error": "无法解码音频：..." }
```

| 状态码 | 语义 | 触发示例 |
|---|---|---|
| `400` | 无法解码 / 参数非法 | 字节不是可解析的音频 |
| `413` | 超出引擎可处理范围 | 音频过长 |
| `415` | 不支持的音频格式 | `Content-Type` 或容器格式不认识 |
| `422` | 可解码但无有效语音 | 静音、纯噪音、无说话人（**次要路径**：开 VAD 后此类输入通常直接得到 `200` + `text: ""`；`422` 保留给引擎显式报告"无音频流"的情况） |

**调用方判定**：归类 `Rejected` → 落"没能听清"标记 → **照常起轮次**，由模型自己决定怎么回应（"刚没听清，你再说一遍？"之类，措辞由模型生成）。

### 5xx — 我们自己的问题（能力层面）

| 状态码 | `kind` | 语义 |
|---|---|---|
| `503` | `engine_unavailable` | ASR 引擎未安装 / 模型加载失败 / `ASR_ENGINE` 未配置 |
| `500` | — | 网关内部错误 |

```json
{ "error": "ASR 引擎不可用：faster-whisper 未安装（pip install faster-whisper）", "kind": "engine_unavailable" }
```

**调用方判定**：归类 `Skipped` → **不落库、不起轮次**，`tracing::error!` 记录 `chat_id` 与原因。MUST NOT 产生任何面向用户的文本。

连接失败 / 读超时（超过 `VOICE_TRANSCRIBE_TIMEOUT`）同样归类 `Skipped`。

### 其他端点（本次扩展）

```
GET /health
```

响应新增一段（**不加载模型**，仅做可导入性探测，避免健康检查触发 GB 级加载）：

```json
{
  "ok": true,
  "diffusion": "comfyui",
  "tts": "sapi",
  "asr": { "engine": "faster-whisper", "importable": true, "model_loaded": false }
}
```

| 字段 | 说明 |
|---|---|
| `asr.engine` | 当前配置的引擎名；`none`/空表示网关不提供转写 |
| `asr.importable` | `importlib.util.find_spec` 结果——引擎模块是否可导入。**不代表模型已就绪** |
| `asr.model_loaded` | 模型是否已加载进内存。启动预热（`ASR_WARMUP=true`）完成前为 `false`；预热失败也为 `false`，但进程继续提供服务（`/transcribe` 会再试一次） |

`sa-link` 启动时读取该段并打进日志，使能力状态在**启动时**就明确可见（FR-013）。

---

## 兼容性约束

- **既有端点行为 MUST NOT 变化**：`/health` 的既有字段（`ok`/`diffusion`/`tts`）保持原样与原类型；`/generate/image`、`/generate/voice` 的请求/响应契约不变。
- **网关在无 ASR 依赖时 MUST 正常启动**：`faster-whisper` 未安装不得影响进程启动与出图/出语音能力（懒加载，失败收敛在 `/transcribe`）。
- **解释器与依赖（安装前提，Spike 实测）**：
  - 解释器 MUST 为 **uv 安装的 CPython 3.13**（`uv python install 3.13`）。系统默认 3.15 与 `py -3.13` 均不可用。
  - 依赖 MUST 锁 **`av<19`**。`av 19.x` 移除了 `av.open(metadata_errors=...)`，faster-whisper 1.2.1 仍在传该参数 → 装上即崩溃（`TypeError`）。
  - 转写 MUST 传 **`vad_filter=True`**（见上）。

## 测试契约（`media-gateway/tests/test_asr.py`）

必须覆盖且**不依赖真实安装 faster-whisper**（通过注入假引擎）：

1. `ASR_ENGINE=none` → `/transcribe` 返回 `503`，`kind == "engine_unavailable"`。
2. 引擎模块不可导入 → `503`，且**不抛出未捕获异常**导致进程崩溃。
3. 假引擎返回空串 → `200` + `text == ""`（不是 4xx）。
4. 假引擎抛出"无法解码"类错误 → `400`。
5. 假引擎抛出"音频过长"类错误 → `413`。
6. 假引擎正常返回 → `200` + `text` 透传，`language`/`duration` 可选带出。
7. `/health` 在引擎不可导入时不报错，且 `asr.importable == false`。
8. **引擎调用参数断言**：分派层传给引擎的调用 MUST 含 `vad_filter=True`（用假引擎记录收到的 kwargs 断言）。这是防幻觉回归的唯一自动化手段。
9. **预热不阻塞启动**：`ASR_WARMUP=true` 且引擎加载抛异常时，进程仍能启动、`/health` 仍为 `200`、`asr.model_loaded == false`。
