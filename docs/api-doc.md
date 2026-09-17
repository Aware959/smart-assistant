# smart-assistant API 文档

后端服务（Axum）接口文档。默认服务地址 `https://127.0.0.1:3000`（可用 `SMART_ASSISTANT_ADDR` 覆盖；无证书时回退纯 HTTP）。全部为 JSON；对话接口为 SSE / WebSocket 流式输出。

## HTTP 接口

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | /chat/stream | 对话 `{message, session_id?, history_count?}`；`session_id` 缺省自动新建；上下文自动取该会话最近 `history_count` 条历史消息（缺省 6）；回复以 **SSE** 实时推送，格式与 OpenAI `chat.completion.chunk` 兼容 |
| POST | /sessions | 手动新建会话 |
| GET  | /sessions | 列出会话（按最近更新倒序） |
| GET  | /sessions/{id}/messages | 某会话的消息记录 |
| DELETE | /sessions/{id} | 删除会话及消息（记忆保留） |
| POST | /memory | 手动添加记忆 `{content, memory_type?, message_id?}`（缺省 fact） |
| GET  | /memories | 列出记忆 |
| DELETE | /memory/{id} | 删除记忆 |
| POST | /search | 语义检索 `{query, limit?}` |
| GET  | /ws | WebSocket 对话：回复文本逐段实时下发，结束时报完整 `ChatOutput` JSON |

（实体/关系图谱为早期版本遗留，接口已下线或弃用，不再列示。）

## 数据模型

| 表 | 内容 | 说明 |
|----|------|------|
| sessions | 会话（id / title / 时间戳） | 标题来自首条消息 |
| messages | 消息记录（role: user/assistant） | 只作记录，不一定产生记忆 |
| memories | 记忆/事实（content / memory_type / tier / expires_at / message_id） | LLM 判定 `is_memory` 时才沉淀；`tier`=short（短期）\| intent（意向）\| core（长期），short/intent 有 `expires_at`，过期不参与召回 |

对话流程：用户消息 → 消息入库 → 记忆向量检索（失败降级为空召回）→ 重组提示词 → 流式回复 → 回复入库；**回复交付之后**再做记忆沉淀：一次极窄的 LLM 调用判定标签（`is_memory` / `memory_type` / `tier` / `relation`），判定为"值得记住"时再调一次做事实化，最后写入 `memories`。

沉淀刻意后置：判定与事实化都要调 LLM，挂在对话路径上会拉长首字延迟，失败还会让用户拿不到回复。记忆链路（召回 / 判定 / 事实化 / 落库）任何环节失败都只记 warn 并降级，不影响对话本身。

## 上下文构建

- 每轮对话自动从数据库取出该会话**最近的历史消息**拼入提示词（含历史的 assistant 回复）；
- 数量由请求参数 `history_count` 控制，`0` 表示不带历史，缺省 `6`；
- 请求里也可显式传 `history`（`[{role, content}]`）作为补充上下文，会拼在自动历史之后；
- 与当前输入语义相关的记忆（`POST /search` 同一套召回，按 `MEMORY_RECALL_THRESHOLD` 过滤）注入 system 提示词。

## SSE 实时输出（/chat/stream，OpenAI 兼容）

```bash
curl -N -X POST http://127.0.0.1:3000/chat/stream \
  -H "Content-Type: application/json" \
  -d '{"message":"你好"}'
```

- 常规数据行均为 OpenAI `chat.completion.chunk`：`data: {"choices":[{"delta":{"content":"..."},...}],"object":"chat.completion.chunk",...}`
- 结尾 `data: [DONE]`（OpenAI SDK / EventSource 可直接按标准流解析）
- 附加元数据事件 `event: done`：`data: <ChatOutput JSON>`（含 `session_id` / `user_message_id` / `memory`；`memory` 为 null 表示本轮没有值得沉淀的事实，非标准扩展，标准客户端会忽略）
- 出错时流内返回 `data: {"error":{"message":"...","type":"assistant_stream_error"}}`，随后仍有 `[DONE]` 与 `done` 事件

## WebSocket（/ws）

对话请求后，回复文本按 chunk 逐段下发，结束时下发完整 `ChatOutput` 的 JSON。

## 环境变量配置

模板见 `backend/.env.example`（复制为 `backend/.env`）。优先级：系统环境变量 > `.env` 文件 > 代码默认值。

| 变量 | 默认值 | 说明 |
|------|--------|------|
| SMART_ASSISTANT_ADDR | 127.0.0.1:3000 | 监听地址 |
| SMART_ASSISTANT_DB | smart_assistant.db | SQLite 路径（相对 backend/ 运行目录） |
| RUST_LOG | info | 日志级别 |
| LLM_API_URL | http://127.0.0.1:1234/v1/completions | OpenAI 兼容接口 |
| LLM_API_KEY | (空) | Key 留空时回退 `OPENAI_API_KEY` |
| LLM_MODEL | google/gemma-4-26b-a4b-qat | 对话模型 |
| LLM_EXTRACT_MODEL | (空) | 记忆提取专用模型；留空则复用 LLM_MODEL |
| PERSONA | (空) | 角色设定，硬约束注入每轮对话与主动推送 |
| EMBEDDING_API_URL | http://127.0.0.1:1234/v1/embeddings | 向量模型接口 |
| EMBEDDING_API_KEY | (空) | 留空时回退 `OPENAI_API_KEY` |
| EMBEDDING_MODEL | text-embedding-embeddinggemma-300m | 向量模型 |
| EMBEDDING_DIM | 768 | 向量维度；变更需 `smart-assistant-memory rebuild` |
| SMART_ASSISTANT_TLS_CERT | certs/server.pem | 证书 PEM（缺省回退运行目录 certs/） |
| SMART_ASSISTANT_TLS_KEY | certs/server.key | 私钥 PEM（缺省回退运行目录 certs/） |
| SMART_ASSISTANT_WEB_DIST | ../frontend/dist | 前端 SPA 静态目录，不存在则不托管 |
| TELEGRAM_BOT_TOKEN | (空) | Telegram Bot Token；留空不启动 TG 长轮询 |
| TELEGRAM_ALLOWED_IDS | (空) | TG 白名单 chat id（逗号分隔），留空不限制 |
| TELEGRAM_PROXY | (空) | TG 代理；留空依次尝试 HTTPS_PROXY / ALL_PROXY |
| ILINK_ENABLED | 0 | 微信 iLink 通道开关 |
| ILINK_BOT_TOKEN | (空) | 手动指定 bot_token 可跳过扫码 |
| ILINK_SESSION_FILE | ilink_session.json | 会话凭证文件 |
| ILINK_ALLOWED_IDS | (空) | iLink 白名单用户 id |
| ILINK_CONTEXT_MAX_AGE_HOURS | 12 | iLink token 新鲜窗口（小时） |
| PROACTIVE_ENABLED | 0 | 主动陪伴总开关 |
| PROACTIVE_MIN_MINUTES | 45 | 主动间隔最小值（分钟） |
| PROACTIVE_MAX_MINUTES | 180 | 主动间隔最大值（分钟） |
| PROACTIVE_QUIET_HOURS | 23-7 | 安静时段（HH-HH 半开区间，23:00~6:59 不主动；起止相同=无安静时段） |
| PROACTIVE_DAILY_LIMIT | 8 | 每用户每天主动消息上限 |
| PROACTIVE_MIN_SILENCE_HOURS | 2 | 距用户最后发言静默多少小时后才可能被主动联系 |
| PROACTIVE_MAX_IDLE_DAYS | 7 | 距用户最后发言超过多少天不再主动打扰 |
| PROACTIVE_MAX_FOLLOWUPS | 3 | 每轮回复后最多连续追加的语句数（0=关闭延续能力） |
| MEMORY_RECALL_THRESHOLD | 0.9 | 召回相似度阈值（欧氏距离 ≤ 该值） |
| MEMORY_DEDUP_THRESHOLD | 0.25 | 记忆去重阈值（距离 ≤ 该值视为同一事实，刷新不新增） |
| MEMORY_SHORT_TTL_DAYS | 7 | short 记忆存活天数 |
| MEMORY_INTENT_TTL_DAYS | 90 | intent 记忆存活天数 |