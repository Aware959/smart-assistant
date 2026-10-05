# smart-assistant API 文档

后端服务（Axum）接口文档。默认服务地址 `https://127.0.0.1:3000`（可用 `SMART_ASSISTANT_ADDR` 覆盖；无证书时回退纯 HTTP）。全部为 JSON；对话接口为 SSE / WebSocket 流式输出。

## HTTP 接口

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | /chat/stream | 对话 `{message, session_id?, history_count?}`；`session_id` 缺省自动新建；上下文自动取该会话最近 `history_count` 条历史消息（缺省 6）；回复以 **SSE** 实时推送，格式与 OpenAI `chat.completion.chunk` 兼容 |
| GET  | /ws | WebSocket 对话：回复文本逐段实时下发，结束时报完整 `ChatOutput` JSON |

> 会话 / 消息 / 记忆的**管理端点已整体移除**（`/sessions`、`/sessions/{id}/messages`、`/memories`、
> `/memory`、`/search`）。理由：记忆 / 情景状态是 core 端口的实现，单独对 Web 暴露管理面会让
> "存储实现"变成公共 API，破坏分层。会话由 `/chat/stream` 首轮自动创建（`session_id` 缺省），
> 数据只经对话路径读写。
>
> （实体/关系图谱为早期版本遗留，接口已下线或弃用，不再列示。）

## 数据模型

| 表 | 内容 | 说明 |
|----|------|------|
| sessions | 会话（id / title / 时间戳） | 标题来自首条消息 |
| messages | 消息记录（role: user/assistant/tool） | 只作记录，不一定产生记忆；带通道时间戳时按**用户真实发出时刻**入库 |
| memories | 记忆/事实（content / memory_type / expires_at / message_id） | 模型调 `remember` 工具主动沉淀；`memory_type` = trait（对方是谁，永不过期）\| preference（偏好，永不过期）\| routine（作息，90 天）\| plan（进行中的事，30 天），routine/plan 有 `expires_at`，过期不参与召回 |
| self_profile | AI 自我属性 KV（key / value / updated_at） | 人设数据源（性格/人格/三观/爱好…），注入 as 提示词；只读不改 |

对话流程：用户消息 → 消息入库 → 取历史 + 组装 system 提示词（**不含记忆**）→ 工具循环（流式生成 → 执行工具 → 回填，直到本轮没有 `tool_calls`）→ 返回。

循环里模型可调四个工具：`send_message`（唯一对外出口，一次调用发一条，即时投递并单独入库）、`get_now`、`recall_memory`（按需检索长期记忆）、`remember`（沉淀一条记忆）。

记忆**按需召回**而非每轮无条件灌入：每轮不再白跑一次 embedding + 向量检索，也不再每轮塞进 5 条大概率无关的记忆。模型不确定用户相关信息时调 `recall_memory`，工具描述里写明"不要凭空猜测"兜住漏调。记忆链路（召回 / 写入）任何环节失败都只回填一次"没做到"让模型自行纠正，不影响对话本身。

## 上下文构建

- 每轮对话自动从数据库取出该会话**最近的历史消息**拼入提示词（含历史的 assistant 回复）；
- 数量由请求参数 `history_count` 控制，`0` 表示不带历史，缺省 `6`；
- 请求里也可显式传 `history`（`[{role, content}]`）作为补充上下文，会拼在自动历史之后；
- 记忆不进提示词，改由模型调 `recall_memory` 按需检索（按 `MEMORY_RECALL_MIN_SIMILARITY` 过滤）；
- **人设注入**：system 提示词开头按最高优先级硬约束注入 AI 自我属性
  （`self_profile` KV 表按插入序渲染，如 `性格：热情、开放；人格：INFP；爱好：…`），
  表为空则整块省略；对话与主动开口判断复用同一份渲染。属性变更直接改表
  （v1 人工维护，不提供管理入口）。
- **输出形状由示例承载**：system 提示词用「身份 + 3 条变长示例」（短/长/短）
  表达回复形态，而非"不要换行""不超过 N 字"这类禁令——模型对禁令的抑制力弱于
  示例分布。示例块内部**不留空行**（空行会被模型当成"回复后留白"的排版模板）。
  不设字数硬上限（硬截断会产生半句话）；流式生成另有 `max_tokens=2048` 兜底异常
  跑飞，不参与塑形。
- **流内空白归一化**：提示词是概率约束，兜不住全部（实测 DeepSeek-V4.1-Flash 仍有
  16% 回复带空行）。故 SSE / WebSocket / CLI 逐段下发的每个增量在推送前都会过一遍
  归一化：连续空白（含 `\n\n`）收敛成一个分隔符、首尾空白裁掉、中文两侧不补空格。
  **逐段下发看到的就是最终入库的**，两者不会不一致。归一化只动空白，不截断不删词。
- **离线积压消息**：通道长轮询的拉取游标持久化在 `meta` 表，重启后续传而非丢弃。
  取回的消息按真实发出时刻分三档处理：≤15 分钟正常回；15 分钟~24 小时在
  system 提示词追加时差说明（让模型知道"这是几小时前发的"，由它自己决定怎么接，
  不许假装刚收到）；>24 小时只入库留痕不回复。主动开口的静默窗口同样按**真实
  发出时刻**计算。详见架构文档 §2.1。
- **主动开口不在 HTTP 路径上**：由 `sa-link` 的意图循环按随机间隔驱动（`INTENT_ENABLED=1`），
  候选与时间账本由 `messages` 派生，无独立状态表。详见架构文档 §4。

## SSE 实时输出（/chat/stream，OpenAI 兼容）

```bash
curl -N -X POST http://127.0.0.1:3000/chat/stream \
  -H "Content-Type: application/json" \
  -d '{"message":"你好"}'
```

- 常规数据行均为 OpenAI `chat.completion.chunk`：`data: {"choices":[{"delta":{"content":"..."},...}],"object":"chat.completion.chunk",...}`
- 结尾 `data: [DONE]`（OpenAI SDK / EventSource 可直接按标准流解析）
- 附加元数据事件 `event: done`：`data: <ChatOutput JSON>`（含 `session_id` / `user_message_id` / `memory`；`memory` 为本轮模型通过 `remember` 写入的最后一条记忆，无写入时为 null。非标准扩展，标准客户端会忽略）
- 流里出现的工具调用不会外泄为正文：模型想说的话只能走 `send_message`，其文本按一条消息逐条推送
- 出错时流内返回 `data: {"error":{"message":"...","type":"assistant_stream_error"}}`，随后仍有 `[DONE]` 与 `done` 事件

## WebSocket（/ws）

对话请求后，回复文本按 chunk 逐段下发，结束时下发完整 `ChatOutput` 的 JSON。

## 环境变量配置

模板见 `backend/.env.example`（复制为 `backend/.env`）。优先级：系统环境变量 > `.env` 文件 > 代码默认值。

| 变量 | 默认值 | 说明 |
|------|--------|------|
| SMART_ASSISTANT_ADDR | 127.0.0.1:3000 | 监听地址 |
| SMART_ASSISTANT_DB | smart_assistant.db | SQLite 路径（相对 backend/ 运行目录） |
| RUST_LOG | info | 日志级别（error/warn/info/debug/trace） |
| SMART_ASSISTANT_LOG_FILE | (空) | 常规文本日志额外镜像到该文件；留空只输出 stdout |
| SMART_ASSISTANT_TRACE_FILE | (空) | 链路追踪 JSON 落盘路径（一行一事件）；留空不写 |
| SMART_ASSISTANT_WATCHDOG_FILE | (空) | 进程假死诊断心跳文件（独立 OS 线程每 5s 一行）；留空不写 |
| LLM_PROVIDER | openai | 提供方：openai（OpenAI 兼容端点）/ google（Gemini 原生协议） |
| LLM_API_URL | http://127.0.0.1:1234/v1/completions | OpenAI 兼容接口 |
| LLM_API_KEY | (空) | Key 留空时回退 `OPENAI_API_KEY` |
| LLM_MODEL | google/gemma-4-26b-a4b-qat | 对话模型 |
| LLM_STRUCTURED_OUTPUT | json_schema | 结构化输出模式：json_schema（强约束）/ json_object（仅提示约束） |
| EMBEDDING_API_URL | http://127.0.0.1:1234/v1/embeddings | 向量模型接口 |
| EMBEDDING_API_KEY | (空) | 留空时回退 `OPENAI_API_KEY` |
| EMBEDDING_MODEL | text-embedding-embeddinggemma-300m | 向量模型 |
| EMBEDDING_DIM | 768 | 向量维度；变更需 `cargo run --bin sa-mem -- rebuild` |
| SMART_ASSISTANT_TLS_CERT | certs/server.pem | 证书 PEM（缺省回退运行目录 certs/） |
| SMART_ASSISTANT_TLS_KEY | certs/server.key | 私钥 PEM（缺省回退运行目录 certs/） |
| SMART_ASSISTANT_WEB_DIST | ../frontend/dist | 前端 SPA 静态目录，不存在则不托管 |
| TELEGRAM_BOT_TOKEN | (空) | Telegram Bot Token；留空不启动 TG 长轮询 |
| TELEGRAM_ENABLED | 1 | 硬开关：0 = 明确不启动（仍需 token 非空） |
| TELEGRAM_ALLOWED_IDS | (空) | TG 白名单 chat id（逗号分隔），留空不限制 |
| TELEGRAM_PROXY | (空) | TG 代理；留空依次尝试 HTTPS_PROXY / ALL_PROXY |
| ILINK_ENABLED | 0 | 微信 iLink 通道开关 |
| ILINK_BOT_TOKEN | (空) | 手动指定 bot_token 可跳过扫码 |
| ILINK_SESSION_FILE | ilink_session.json | 会话凭证文件 |
| ILINK_ALLOWED_IDS | (空) | iLink 白名单用户 id |
| ILINK_CONTEXT_MAX_AGE_HOURS | 12 | iLink token 新鲜窗口（小时） |
| INTENT_ENABLED | 0 | 主动开口总开关；只在 `sa-link` 生效，`sa-web` 不跑意图循环 |
| INTENT_MIN_MINUTES | 45 | 随机间隔下界（兼作距上次 assistant 发言的最小间隔） |
| INTENT_MAX_MINUTES | 180 | 随机间隔上界 |
| INTENT_QUIET_HOURS | 23-7 | 安静时段（HH-HH 半开区间，23:00~6:59 不主动；起止相同=无安静时段） |
| INTENT_MIN_SILENCE_HOURS | 2 | 距用户最后发言静默多少小时后才可能被主动联系 |
| INTENT_MAX_IDLE_DAYS | 7 | 距用户最后发言超过多少天不再主动打扰 |
| MEMORY_RECALL_MIN_SIMILARITY | 0.79 | 召回的最低余弦相似度（≥ 该值才入选） |
| MEMORY_DEDUP_MIN_SIMILARITY | 0.92 | 去重的最低余弦相似度（≥ 该值视为同一事实，改写不新增）；必须大于召回阈值 |
| TRACE_CONTENT | 0 | 1 = 追踪日志记录内容全文；缺省只记摘要（条数/字符数） |