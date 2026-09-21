# SMART Assistant 架构文档

> 本文描述后端 Rust 内核的分层结构、核心数据流与各子系统的协作方式。
> 
> 上一次更新：2026-09-21

---

## 1. 分层架构

```
┌─────────────────────────────────────────────────────────────────────┐
│  传输层（外部适配层，经 bin 接线）                                     │
│  ┌──────────┐  ┌──────────────┐  ┌────────────────┐                │
│  │ HTTP/WS  │  │ Telegram     │  │ iLink 微信     │                │
│  │ /chat/   │  │ long-poll    │  │ WS relay       │                │
│  │ stream   │  │ polling      │  │                │                │
│  └────┬─────┘  └──────┬───────┘  └───────┬────────┘                │
│       │ Host         │ Host + ChannelsPusher（PushPort 实现）      │
├───────┴───────────────┴──────────────────┴──────────────────────────┤
│  应用本体：core/（具体类型直连，只对三个能力口编程）                    │
│  app.rs      组合根 Assistant：开 db、装配会话/主动簿/能力口、实现 Host │
│  host.rs     窄端口 Arc<dyn Host>（无 Host::db）                     │
│  agent_run.rs 单轮流水线 chat_stream() + 记忆沉淀 settle_memory()（§2）│
│  session/    Sessions 会话内存运行时（首碰水合 + 变化即写穿）          │
│  runtime/    ProactiveBook 主动状态簿 + realtime 统一"现在"时间源     │
│  memory/     AI 记忆：Store（MemoryStore）/ user_memory_extraction    │
│                + current_relation / current_phase（纯实现，详见 §3） │
│  proactive/  主动陪伴引擎（调度/决策/推送/延续，外发经 PushPort）      │
│  context/    上下文组装：历史时间线 / 记忆召回的取数与渲染             │
│  ports.rs    能力口：ChatLlm / MemoryStore / PushPort                │
│  types.rs    ChatInput/Output、AgentContext、Extraction、MemoryRecord/│
│              Hit、MessageTurn、ProactiveCandidate、RelationEvent      │
│  旧"存储端口"（ConversationStore / SessionMap / ProactiveStore /     │
│  Embedder）已删除——core 与 db 直接接线，会话/状态簿为本体 inherent     │
├──────────────────────────────────────────────────────────────────────┤
│  能力层（实现 core 的能力口）                                          │
│  llm/chat.rs       → ChatLlm   （complete/complete_structured/stream）│
│  llm/embedding.rs  → embed_text()（genai 阻塞 API，spawn_blocking）    │
│  channels/ChannelsPusher → PushPort（外发派发，只读 PushChannels 注册表）│
│  core/memory/Store → MemoryStore                                     │
├──────────────────────────────────────────────────────────────────────┤
│  存储层：db/mod.rs（rusqlite + sqlite-vec）                            │
│  messages / memories / memory_vectors / sessions / channel_sessions / │
│  proactive_state / time_profiles / world_state / relations / meta      │
│  （time_profiles / world_state / relations 表按 schema 保留、暂无代码   │
│   接线——aiworld 重设计后由新适配器写）                                │
│  ↕ Mutex<Database> 同步访问（经 core/session、core/runtime 适配器收敛）│
├──────────────────────────────────────────────────────────────────────┤
│  配置层：config.rs（.env 环境变量 → OnceLock<Config>，全程单例）          │
└──────────────────────────────────────────────────────────────────────┘
```

---

## 2. 核心数据流：一条消息的全旅程

以 Telegram 用户发来一条消息为例（HTTP/SSE/iLink 路径一致，仅通道接入不同）：

```
TG 长轮询收到 Update
  │
  ├─ ts_to_rfc3339(msg.date) → 归一化为 RFC3339 用户真实发出时刻
  │  （路径：channels/mod.rs ts_to_rfc3339）
  │
  ├─ Host::ensure_session("telegram", chat_id)
  │  → core/session::Sessions.resolve() 内存通道映射命中即复用，未命中建映射（即写穿）
  │
  ├─ Sessions::add_message_at(session_id, "user", text, user_time)
  │  → 内存缓冲追加 + 即写穿 messages 表
  │  （路径：core/agent_run.rs，有通道时间戳时用之，否则记接收时刻）
  │
  ├─ ① 记忆召回（语义向量检索，≤ threshold 才入选，走 MemoryStore 能力口）
  │     core::agent_run → core/memory::search(db, text, MEMORY_RECALL_LIMIT=5)
  │       → llm::embedding::embed_text(text) → embedding API（本地 1234 端口）
  │       → db_memory::search_similar() → vec0 kNN（k = ? 约束）
  │       → 按 memory_recall_threshold 过滤，剔除已过期记忆
  │       → 失败只降级为空召回（记忆是辅助能力，不阻断对话）
  │
  ├─ ② 上下文组装（core::context，只读；与主动决策共用同一套实现）
  │     context::fetch_history(conversation: &Sessions, session_id, history_count)
  │       ├─ 默认按"今天"取消息（日界 = 固定东八区 06:00；今日 <6 条自动并入昨天）
  │       │   （时区偏移此前由世界运行时推断，aiworld 拆出后暂用固定常量，待重设计）
  │       ├─ >60 条 → 保留头 8 条 + 尾 + 省略中间
  │       ├─ 内容保持原文，不带时间/角色前缀
  │       ├─ history_count 显式指定 → 退化为取最近 N 条
  │       └─ ⚠ 必须在"本条用户消息入库之前"取，否则本轮消息会被算进历史
  │     context::system_prompt(recall)
  │       ├─ 角色设定：PERSONA 环境变量（中文原文注入，最高优先级）
  │       ├─ 英文行为指令：对方中心 / 格式禁令 / 长度约束 / 身份边界
  │       └─ 召回记忆文本（"Below is what you know about the other person..."）
  │
  ├─ ③ 流式生成回复（走 ChatLlm 端口）
  │     llm::chat::complete_stream(messages, on_delta)
  │       → genai_client: 按 LLM_PROVIDER 选 OpenAI / Gemini adapter
  │       → genai 0.6.5 流式 API
  │       → delta 逐段回调 on_delta（SSE 逐块推送 / CLI 实时打印）
  │
  ├─ ④ 回复入库，刷新会话时间戳
  │     Sessions::add_message(session_id, "assistant", reply) + touch_session(session_id)
  │     → 内存缓冲 + 即写穿
  │     （core/agent_run 内全部 inherent 方法，不再有 ConversationStore 端口）
  │     → Agent::chat_stream 到此返回（带 session_id / reply / user_message_id）
  │
  └─ ⑤ 记忆沉淀（**回复交付之后**，宿主调 Agent::settle_memory，走 MemoryStore 能力口）
├─ 判定：memory::user_memory_extraction::judge(text) → 极窄 JSON 标签，走 json_schema / json_object
  │     { is_memory, memory_type, tier, relation }（schema 用 enum 收窄）
  │     只做分类、不生成自然语言，输出维度小 → 模型几乎写不歪
  │     （relation 标签仍产出并随记忆提取保留，关系演化接线已随 aiworld 拆出，重设计后恢复）
        ├─ 事实化：仅当 is_memory=true 才追加一次调用
│        user_memory_extraction::summarize(text) → 第三人称事实句（纯文本，不是 JSON）
        └─ 落库：memory::store(...) → memories 行 + memory_vectors（去重/分层/TTL）
              → llm::embedding::embed_text(content) → 向量化
              → 去重：与已有记忆距离 ≤ dedup_threshold → 只刷 updated_at，不新增
              → 插入 memories + memory_vectors
        任何环节失败只记 warn 并降级（不写记忆），绝不影响已交付的回复
```

---

## 3. AI 自记忆域——已整体拆出，等待重新设计

AI "关于自己的记忆"（性格/情绪/关系/作息画像，结构化运行量，非可召回事实）此前由
`StateStore` 端口 + `runtime::World` 内存适配器 + 心跳推进支撑。**该世界（aiworld）
已按决策整体拆出**（下表中的 `runtime/`、`memory/` 均为旧路径，现布局在 `core/runtime`、
`core/memory`）：

| 拆出项 | 状态 |
|--------|------|
| `core::ports::StateStore` 端口 | 已删除 |
| `runtime/state.rs`（World：水合 + 写穿 + 世界引擎推进） | 已删除 |
| `runtime/session.rs` 的 `user_hours_of`（作息画像喂数，会话今已移入 `core/session/`） | 已删除 |
| `memory/ai_state.rs`（自我档案 `SelfState` / `DEFAULT_SELF_BASE`） | 已删除 |
| `memory/ai_realtime_mood.rs`（情绪 `Mood` / 回落） | 已删除 |
| `memory/current_state_assemble.rs`（快照 `Snapshot` / `render`） | 已删除 |
| `db/world_state.rs` / `db/time_profile.rs`（写穿模块） | 已删除 |
| `Host::world_snapshot_text` / `Host::tick_state`、`bin/server` 世界心跳 | 已删除 |
| `core/context/state.rs`（`render_state` 取数封装） | 已删除 |
| **保留** `memory/current_relation.rs` / `memory/current_phase.rs`（纯值 + 演化规则） | 纯实现留存 |
| 表 `world_state` / `relations` / `time_profiles` | 按 schema 只增约定保留，暂无代码接线 |

> 附带一处既有决策：此前的"今日叙事"（narrative）随世界一并移除——它曾因
> `updated_at` 每跳刷新而被 120 分钟间隔门永久卡死，实际从未生效，其列仅按
> schema 只增保留。
>
> 历史上下文"今天"日界的时区偏移原由世界运行时按画像动态推断，现改为固定东八区
> 常量（`core/context/history.rs` 的 `USER_OFFSET_MINUTES`），重设计后恢复动态推断。
>
> 重新设计定稿后：端口契约、内存适配器与写穿回归 `runtime`，`current_relation` 的
> `apply_event`（由记忆提取的 `relation` 标签触发）与 `current_phase` 的 `infer_from_hours`
> 直接复用既有纯实现。

---

## 4. 主动陪伴引擎（proactive/）

与世界引擎解耦，依赖 `PROACTIVE_ENABLED=1` 且至少有一个通道启用。

### 4.1 主循环

```
proactive::run(assistant, pusher)   ← pusher = Arc<dyn PushPort>（bin 注入 ChannelsPusher）
  loop {
      idle = scheduler::next_sleep()        ← 随机间隔（安静时段则直接睡到结束）
      sleep(idle)
      run_once(assistant, pusher)           ← 每轮最多发出一条主动消息
  }
```

### 4.2 单轮评估（run_once）

```
1. in_quiet_hours() → 静默跳过
2. ProactiveBook::eligible_candidates() → 内存状态簿 + proactive/store 过滤：
     - last_user_reply_at + min_silence_hours ≤ now（用户沉默够久）
     - today_count < daily_limit（当日未超限）
     - （iLink 还需 context_token 在新鲜窗口内）
3. 对每个候选（每轮最多 MAX_CANDIDATES=5）：
     - sender::pushable(pusher, cand) → PushPort 查询通道是否可推（TG：已连通；iLink：token 未过期且发送次数未超）
     - propose() → 组装上下文（经 Host：最近历史 + 记忆召回）→ LLM 开口决策（JSON）
     - sender::dispatch(assistant, pusher, cand, text) → 经 PushPort 外发（Telegram / iLink）
     - 发出即 return（一轮只发一条，避免轰炸）
```

### 4.3 延续追加（continuation）

每次正常回复后（含主动回复），由 `continuation::propose_follow_up()` 按
`proactive_max_followups` 限制的次数决定是否像真人一样接着补一两句
（话题有延展性时）。追加句是"对话"而非"主动发起"，**不计入每日主动配额**
（原文档此处表述有误，一并修正）。continuation 由各 relay 传入 `&ChannelsPusher`
复用同一能力口。

---

## 5. 时间世界——已随 aiworld 拆出

原 `World::observe()`（消息入库后重算作息画像）与 `time_profiles` 写穿随 aiworld
整体移除。`memory/current_phase.rs` 的 `infer_from_hours` 纯推断逻辑保留，重设计后
再由新适配器（喂数、水合、写穿）接线。

按天上下文构建（`core/context/history.rs` 的 `build_day_history`）的关键常量：

| 常量 | 值 | 含义 |
|------|----|------|
| `DAY_START_HOUR` | 6 | 当地 06:00 为一日起点（当前按固定东八区偏移） |
| `DAY_POOL_THRESHOLD` | 6 | 今日 <6 条时，自动并入昨天 |
| `DAY_CONTEXT_MAX` | 60 | 超出此条数时压缩 |
| `DAY_KEEP_HEAD` | 8 | 压缩时保留的头部条数 |

---

## 6. 记忆系统（core/memory/）

### 6.1 分层与 TTL

| tier | 含义 | 默认 TTL |
|------|------|----------|
| `core` | 长期稳定事实与偏好 | 永不过期 |
| `intent` | 计划/进行中的事 | 90 天（`memory_intent_ttl_days`）|
| `short` | 临时/短期状态 | 7 天（`memory_short_ttl_days`）|

召回时自动剔除已过期记忆。

### 6.2 向量检索

```
core/memory::search(query)
  │
  ├─ vec_status()?.search_usable() → 向量签名不匹配时静默降级为空
  ├─ llm::embedding::embed_text(query) → EMBEDDING_API_URL（本地 1234 端口，同步阻塞）
  └─ db::memory::search_similar(vector, limit)
       SELECT ... FROM memory_vectors
       WHERE embedding MATCH ?1 AND k = ?3    ← vec0 kNN 查询，必须 k = ?
       ORDER BY distance
       → 按 memory_recall_threshold（欧氏距离 ≤ 阈值）过滤
```

### 6.3 去重

新记忆向量化后与已有记忆算欧氏距离，距离 ≤ `memory_dedup_threshold` 时
视为同一事实，只刷新 `updated_at` 不新增（防止重复信息膨胀）。

---

## 7. 数据库 Schema（v8）

SQLite + WAL 模式 + sqlite-vec 向量扩展。所有业务表仅做增量迁移，绝不 DROP。

| 表名 | 用途 |
|------|------|
| `meta` | 纯键值元信息（schema_version / embedding_dim / embedding_model） |
| `sessions` | 会话（id / title / created_at / updated_at） |
| `messages` | 对话消息（role = user / assistant） |
| `memories` | 记忆事实（content / tier / expires_at / message_id） |
| `memory_vectors` | sqlite-vec0 向量表（memory_id / embedding float32[]） |
| `channel_sessions` | 通道 ↔ 会话映射（channel + external_id → session_id） |
| `proactive_state` | 主动状态簿（今日计数 / 最后主动 / 最后用户回复时间） |
| `time_profiles` | 时间画像（offset_minutes / active_hour / 观测次数）——**保留、暂无线接** |
| `world_state` | AI 世界状态（单行 id=1：自我档案 / 情绪）——**保留、暂无线接** |
| `relations` | 关系状态（closeness / trust）——**保留、暂无线接** |

---

## 8. 配置体系

全部走环境变量，由 `backend/.env`（gitignored，不入库）提供，启动时 dotenvy 加载。
新增任何环境变量**必须同步**到 `backend/.env.example`。

### 8.1 LLM 提供方切换

```env
LLM_PROVIDER=openai          # 或 google（Gemini 原生协议）
LLM_API_URL=https://api.groq.com/openai/v1/completions
LLM_API_KEY=gsk_xxx
LLM_MODEL=qwen/qwen3.8-27b
LLM_EXTRACT_MODEL=qwen/qwen3.8-27b   # 记忆提取专用，留空则复用 LLM_MODEL
```

- `openai` 模式：genai `OpenAIAdapter`（兼容 Groq / DeepSeek / LM Studio 等）
- `google` 模式：genai `GeminiAdapter`（支持 JsonSpec 结构化 + 流式）

### 8.2 通道开关

```env
TELEGRAM_ENABLED=1   # 缺省开启；0 = 明确不启动（仍需 token 非空）
ILINK_ENABLED=0      # 0/留空 = 不启动；1 = 启动（有 token 直连，无 token 扫码）
```

### 8.3 主动陪伴

```env
PROACTIVE_ENABLED=0          # 总开关
PROACTIVE_MIN_MINUTES=45     # 随机间隔最小
PROACTIVE_MAX_MINUTES=180    # 随机间隔最大
PROACTIVE_QUIET_HOURS=23-7   # 安静时段（本机时间，HH-HH，跨午夜取 start > end）
PROACTIVE_DAILY_LIMIT=8      # 每用户每天上限
PROACTIVE_MIN_SILENCE_HOURS=2   # 用户沉默多久才可能被主动联系
PROACTIVE_MAX_IDLE_DAYS=7    # 超过多少天未回复则不再打扰
PROACTIVE_MAX_FOLLOWUPS=3    # 每轮回复后最多连续追加几句话
```

---

## 9. 对外接口

### 9.1 HTTP/WS/SSE（backend/默认端口 3000）

由 axum 框架托管，路由挂载在 `api/mod.rs build_router()`。

| 端点 | 方法 | 说明 |
|------|------|------|
| `/v1/chat/stream` | POST | SSE 流式对话（OpenAI 兼容格式）；最后一条 `event: done` 携带 `ChatOutput JSON` |
| `/v1/chat/stream` | GET  | WebSocket 实时对话（同一流水线，文本触发 → delta 实时下发） |

会话 / 消息 / 记忆的管理端点（`/v1/sessions`、`/v1/memory`、`/v1/memory/search` 等）已随 Web
管理页**整体移除**：记忆 / 情景状态是 core 端口的实现，对 Web 单独开管理面会把"存储实现"
变成公共 API，破坏分层。会话由首轮对话自动创建（`session_id` 缺省）。

SPA 静态文件由 `SMART_ASSISTANT_WEB_DIST` 指定目录，同源托管，未知路径回退 index.html。

### 9.2 移动端（UniFFI）

研发早期先稳定对话与内核：**UniFFI 导出已移除**（`crate-type` 改回纯 rlib，无 cdylib），
`android/` 保持占位。待内核稳定后再引入 `uniffi` 并把 `Assistant` 的对话能力导出给
Kotlin/Swift。

---

## 10. 关键设计决策与约定

1. **DB 同步**：所有数据库访问经 `std::sync::Mutex<Database>` 加锁，简单可靠。
2. **LLM 阻塞调用**：genai 是阻塞 API，在 async 通道里一律 `tokio::task::spawn_blocking`。
3. **消息 vs 记忆**：`messages` 是原始对话记录；`memories` 只沉淀事实，来源用 `message_id` 关联。
4. **Schema 迁移**：只做增量（只增列/表，绝不 DROP 业务表）。旧库补列用 `add_column_if_missing`。
5. **向量签名锁定**：embedding 模型变更不会自动重建，由独立工具 `smart-assistant-memory rebuild` 手动触发，保证旧库不丢数据。
6. **vec0 kNN 约束**：查询必须写 `k = ?`，把 LIMIT 写在外层查询会报 "A LIMIT or 'k = ?' constraint is required"。
7. **JSON 结构化输出**：记忆判定 / 开口决策 / 延续判断均走 json_schema（支持的模型自动约束输出格式，不支持时退化为自由 JSON + 容错解析）。模型输出一律当**不可信输入**：字段缺失 / `null` / 类型不符 / 空白都回退默认值，不让一次格式抖动变成业务失败。
8. **记忆后置 + 全链路降级**：记忆沉淀（判定 / 事实化 / 落库）不挂在对话路径上，由宿主在回复交付之后调用 `Agent::settle_memory`（telegram / iLink 用 `spawn_blocking` 后台跑，SSE / CLI 同步补再把结果回填 `ChatOutput.memory`）。记忆链路的任何环节失败都只记 warn 并降级，绝不冒泡成"对话失败"。
