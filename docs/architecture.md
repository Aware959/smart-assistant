# SMART Assistant 架构文档

> 本文描述后端 Rust 内核的分层结构、核心数据流与各子系统的协作方式。
> 
> 上一次更新：2026-09-14

---

## 1. 分层架构

```
┌─────────────────────────────────────────────────────────────────────┐
│  对外接口层                                                          │
│  ┌──────────┐  ┌──────────────┐  ┌────────────────┐  ┌──────────┐ │
│  │ HTTP/WS  │  │ Telegram     │  │ iLink 微信     │  │ UniFFI   │ │
│  │ /chat/   │  │ long-poll    │  │ WS relay       │  │ Kotlin   │ │
│  │ stream   │  │ polling      │  │                │  │ FFI      │ │
│  └────┬─────┘  └──────┬───────┘  └───────┬────────┘  └────┬─────┘ │
│       │               │                  │                 │       │
├───────┴───────────────┴──────────────────┴─────────────────┴───────┤
│  编排层：agent.rs                                                    │
│  chat_stream() — 单轮对话流水线（见 §2）                             │
├───────────────────────────────────────────────────────────────────────┤
│                        ↙       ↘                                      │
│  ┌─────────────┐   世界引擎     主动陪伴引擎                           │
│  │ world/      │   (60s 心跳)   (随机间隔循环)                        │
│  │ narrative   │   ↘          ↙                                       │
│  │ emotion     │    均读写 DB   均通过 PushChannels 查通道可推状态       │
│  │ relation    │                                                         │
│  └─────────────┘                                                         │
├────────────────────────────────────────────────────────────────────────┤
│  服务层：services/mod.rs                                                │
│  会话 CRUD / 消息 CRUD / 记忆 CRUD（面向 FFI 的序列化视图记录）            │
├────────────────────────────────────────────────────────────────────────┤
│  业务能力层                                                             │
│  ┌──────────────┐  ┌───────────────┐  ┌──────────────────────────┐   │
│  │ memory/      │  │ llm/chat.rs   │  │ embedding/mod.rs         │   │
│  │ extraction   │  │ complete      │  │ embed_text()             │   │
│  │ store        │  │ complete_stream│  │ → EMBEDDING_API_URL      │   │
│  └──────────────┘  └───────────────┘  └──────────────────────────┘   │
│  genai_client.rs — genai 适配（按 LLM_PROVIDER 选 OpenAI / Gemini）     │
├────────────────────────────────────────────────────────────────────────┤
│  存储层：db/mod.rs（rusqlite + sqlite-vec）                              │
│  messages / memories / memory_vectors / sessions / channel_sessions /  │
│  proactive_state / time_profiles / world_state / relations / meta      │
│  ↕ Mutex<Database> 同步访问                                            │
├────────────────────────────────────────────────────────────────────────┤
│  配置层：config.rs（.env 环境变量 → OnceLock<Config>，全程单例）          │
└────────────────────────────────────────────────────────────────────────┘
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
  ├─ db::channel::resolve(db, "telegram", chat_id)
  │  → 首次自动新建 session，后续复用；同时更新 channel_sessions.updated_at
  │
  ├─ db::message::create_at(db, session_id, "user", text, user_time)
  │  （路径：agent.rs:88-96，有通道时间戳时用之，否则记接收时刻）
  │
  ├─ timeworld::observe(db, session_id)   ← 重算用户作息画像（offset_minutes / active_hour）
  ├─ world::relation::observe(db, session_id) ← 每次对方来消息，关系升温
  │
  ├─ ① 记忆提取（结构化 JSON，必填字段）
  │     memory::extraction::extract_from_text(text)
  │       → LLM_EXTRACT_MODEL（或 llm_model）+ json_schema
  │       → 返回 MemoryExtraction { is_memory, content, memory_type, tier }
  │
  ├─ ② 记忆召回（语义向量检索，≤ threshold 才入选）
  │     memory::store::search(db, text, MEMORY_RECALL_LIMIT=5)
  │       → embedding::embed_text(text) → embedding API（本地 1234 端口）
  │       → db_memory::search_similar() → vec0 kNN（k = ? 约束）
  │       → 按 memory_recall_threshold 过滤，剔除已过期记忆
  │
  ├─ ③ 构建提示词
  │     build_system_prompt(recall, world)
  │       ├─ 角色设定：PERSONA 环境变量（中文原文注入，最高优先级）
  │       ├─ 英文行为指令：对方中心 / 格式禁令 / 长度约束 / 身份边界 / 生活感
  │       ├─ 世界状态：timeworld::render()（AI本机时间 / 用户当地推断 / 作息 / 关系 / 叙事）
  │       └─ 召回记忆文本（"Below is what you know about the other person..."）
  │     build_day_history(db, session_id)
  │       ├─ 默认按"今天"取消息（日界 = 用户当地 06:00）
  │       ├─ 每条渲染为 [HH:MM] 用户/AI: 内容
  │       ├─ 同日 >2h 间隔 → 插入 〈沉默 N 小时〉
  │       ├─ 今日 <6 条 → 自动并入昨天
  │       └─ >60 条 → 保留头 8 条 + 尾 + 〈省略中间 N 条〉
  │     （若 history_count 显式指定 → 退化为取最近 N 条纯文本）
  │
  ├─ ④ 流式生成回复
  │     llm::chat::complete_stream(messages, on_delta)
  │       → genai_client: 按 LLM_PROVIDER 选 OpenAI / Gemini adapter
  │       → genai 0.6.5 流式 API
  │       → delta 逐段回调 on_delta（SSE 逐块推送 / CLI 实时打印）
  │
  ├─ ⑤ 回复入库，刷新会话时间戳
  │     db::message::create(db, session_id, "assistant", reply)
  │     db::session::touch(db, session_id)
  │
  └─ ⑥ 记忆沉淀（仅 LLM 判定为值得记住时）
        memory::store::store(db, content, memory_type, tier, Some(message_id))
          → embedding::embed_text(content) → 向量化
          → 去重：与已有记忆距离 ≤ dedup_threshold → 只刷 updated_at，不新增
          → 插入 memories + memory_vectors
```

---

## 3. 世界引擎（world/）

**与对话完全解耦，始终运行**——无论 `PROACTIVE_ENABLED` 是否开启。

### 3.1 心跳循环（60 秒/跳）

| 动作 | 模块 | 说明 |
|------|------|------|
| 情绪回落 | `emotion::tick_mood()` | 按距上次的经过时间向基线回归（valence / energy 两个维度） |
| 关系降温 | `relation::tick_decay()` | 所有关系按距上次经过时间衰减 closeness / trust |
| 叙事推进 | `narrative::advance()` | 仅时段/日期切换时触发；LLM 阻塞调用 → `spawn_blocking` |

### 3.2 叙事推进触发时机

`narrative::should_advance()` 返回 `true` 时才触发 LLM（减少调用）：
- `today_date` 为 None 或 ≠ 今日（跨天，重新起笔）
- `last_phase` 为 None 或 ≠ 当前时段标签（如从"上午"切换到"下午"）

### 3.3 世界状态注入对话

`timeworld::render(db, session_id)` 生成快照文本注入 system prompt：
```
【此刻的世界】
- AI 本机时间：2026-09-14 15:43，下午。
- 对方最后发言：15:43（10分钟前）。
- 推断对方当地现在是：15:43，下午。
- 对方作息：通常在午后这个时段活跃。
- 我的生活设定：（DEFAULT_SELF_BASE 全文）
- 此刻的心境：平静。
- 今天到现在我经历了：（今日叙事文本）
- 我和对方的关系：刚认识，彼此试探中。
```

---

## 4. 主动陪伴引擎（proactive/）

与世界引擎解耦，依赖 `PROACTIVE_ENABLED=1` 且至少有一个通道启用。

### 4.1 主循环

```
proactive::run(assistant)
  loop {
      idle = scheduler::next_sleep()        ← 随机间隔（安静时段则直接睡到结束）
      sleep(idle)
      run_once(assistant)                   ← 每轮最多发出一条主动消息
  }
```

### 4.2 单轮评估（run_once）

```
1. in_quiet_hours() → 静默跳过
2. eligible_candidates() → 从 proactive_state 表筛出：
     - last_user_reply_at + min_silence_hours ≤ now（用户沉默够久）
     - today_count < daily_limit（当日未超限）
     - （iLink 还需 context_token 在新鲜窗口内）
3. 对每个候选（每轮最多 MAX_CANDIDATES=5）：
     - pushable(cand) → 通道是否可推（TG：已连通；iLink：token 未过期且发送次数未超）
     - propose() → 组装上下文（recent_history + recall + world）→ LLM 开口决策（JSON）
     - dispatch() → push_send 到对应通道（Telegram / iLink）
     - 发出即 return（一轮只发一条，避免轰炸）
```

### 4.3 延续追加（continuation）

每次正常回复后（含主动回复），由 `continuation::propose_follow_up()` 按
`proactive_max_followups` 限制的次数决定是否像真人一样接着补一两句
（话题有延展性时）。每条追加句计入通道配额。

---

## 5. 时间世界（timeworld）

### 5.1 画像建立

`timeworld::observe()` 在每次用户消息入库后调用：
- `db::channel::resolve` 拿到 `channel + external_id`
- 从 `time_profiles` 表读取（或新建）观测记录
- 用报文时间戳（或推断）重算 `offset_minutes` 和 `active_hour`
- 累计观测次数，uptsert 回库

### 5.2 推断逻辑

- 若通道报文带 `user_time`（精确时刻），直接解析
- 否则走东八区兜底（`offset_minutes = 480`）
- `active_hour` 用指数滑动平均更新，反映用户活跃时段习惯

### 5.3 按天上下文构建

`build_day_history(db, session_id)` 的关键常量：

| 常量 | 值 | 含义 |
|------|----|------|
| `DAY_START_HOUR` | 6 | 用户当地时间 06:00 为一日起点 |
| `DAY_POOL_THRESHOLD` | 6 | 今日 <6 条时，自动并入昨天 |
| `DAY_CONTEXT_MAX` | 60 | 超出此条数时压缩 |
| `DAY_KEEP_HEAD` | 8 | 压缩时保留的头部条数 |
| `SILENCE_MARK_MINUTES` | 120 | 同日间隔 >2h 插入沉默标记 |

---

## 6. 记忆系统（memory/）

### 6.1 分层与 TTL

| tier | 含义 | 默认 TTL |
|------|------|----------|
| `core` | 长期稳定事实与偏好 | 永不过期 |
| `intent` | 计划/进行中的事 | 90 天（`memory_intent_ttl_days`）|
| `short` | 临时/短期状态 | 7 天（`memory_short_ttl_days`）|

召回时自动剔除已过期记忆。

### 6.2 向量检索

```
memory::store::search(query)
  │
  ├─ vec_status()?.search_usable() → 向量签名不匹配时静默降级为空
  ├─ embedding::embed_text(query) → EMBEDDING_API_URL（本地 1234 端口，同步阻塞）
  └─ db_memory::search_similar(vector, limit)
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
| `time_profiles` | 时间画像（offset_minutes / active_hour / 观测次数） |
| `world_state` | AI 世界状态（单行 id=1：自我档案 / 叙事 / 情绪） |
| `relations` | 关系状态（closeness / trust） |

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
| `/v1/memory/search` | POST | 语义检索记忆 |
| `/v1/memory` | POST / DELETE | 添加 / 删除记忆 |
| `/v1/sessions` | POST / GET | 新建 / 列出会话 |
| `/v1/sessions/:id/messages` | GET | 列出某会话内消息 |

SPA 静态文件由 `SMART_ASSISTANT_WEB_DIST` 指定目录，同源托管，未知路径回退 index.html。

### 9.2 UniFFI（Kotlin FFI）

`Assistant` 通过 `#[uniffi::export]` 导出 Kotlin 方法，供 `android/` 调用。
核心：`new_ffi()` / `chat_ffi()` / `create_session_ffi()` / `list_messages_ffi()` 等。

---

## 10. 关键设计决策与约定

1. **DB 同步**：所有数据库访问经 `std::sync::Mutex<Database>` 加锁，简单可靠。
2. **LLM 阻塞调用**：genai 是阻塞 API，在 async 通道里一律 `tokio::task::spawn_blocking`。
3. **消息 vs 记忆**：`messages` 是原始对话记录；`memories` 只沉淀事实，来源用 `message_id` 关联。
4. **Schema 迁移**：只做增量（只增列/表，绝不 DROP 业务表）。旧库补列用 `add_column_if_missing`。
5. **向量签名锁定**：embedding 模型变更不会自动重建，由独立工具 `smart-assistant-memory rebuild` 手动触发，保证旧库不丢数据。
6. **vec0 kNN 约束**：查询必须写 `k = ?`，把 LIMIT 写在外层查询会报 "A LIMIT or 'k = ?' constraint is required"。
7. **JSON 结构化输出**：记忆提取 / 开口决策 / 延续判断均走 json_schema（支持的模型自动约束输出格式，不支持时退化为自由 JSON + 容错解析）。
