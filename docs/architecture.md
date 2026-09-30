# SMART Assistant 架构文档

> 本文描述后端 Rust 内核的分层结构、核心数据流与各子系统的协作方式。
> 
> 上一次更新：2026-09-30

---

## 1. 分层架构

```
┌─────────────────────────────────────────────────────────────────────┐
│  传输层（外部适配层，经 bin 接线）                                     │
│  ┌──────────┐  ┌──────────────┐  ┌────────────────┐                │
│  │ HTTP/WS  │  │ Telegram     │  │ iLink 微信     │                │
│  │ /chat/   │  │ long-poll    │  │ long-poll      │                │
│  │ stream   │  │ polling      │  │                │                │
│  └────┬─────┘  └──────┬───────┘  └───────┬────────┘                │
│       │ Agent+Sessions │ Agent+Sessions+IntentTracker                │
│       │                │ + ChannelsPusher（主动外发唯一实现）        │
├───────┴───────────────┴──────────────────┴──────────────────────────┤
│  应用本体：core/（具体类型直连，能力以具体类型接线）                    │
│  app.rs      组合根 App：开 db、装配 Sessions / IntentTracker / Llm /  │
│              Store / Profile → 注入 Agent。只装配不转发。             │
│  agent.rs    Agent 单轮流水线 chat_stream() + 记忆沉淀 settle_memory() │
│              （§2）                                                   │
│  session/    Sessions 会话内存运行时（首碰水合 + 变化即写穿）          │
│              + classify_inbound() 入站时效分级（§2.1）                │
│  intent/     主动陪伴引擎：pace 调度 / filter 规则 / speak+continuation│
│              决策 / sender 派发 / tracker 状态簿（§4）                 │
│  memory/     AI 记忆：Store / user_memory_extraction                  │
│              + current_relation / current_phase（纯实现，详见 §3）    │
│  context/    上下文组装：历史时间线 / 记忆召回的取数与渲染             │
│  schema/     ChatInput/Output、AgentContext、Extraction、MemoryRecord/│
│              Hit、MessageTurn、IntentCandidate、RelationEvent          │
│  旧"存储端口"与 Host / ports.rs 窄端口已删除——core 与 db 直接接线，   │
│  会话/状态簿为本体 inherent；唯一反向引用是 ChannelsPusher（主动外发    │
│  属"外部能力"而非传输端点，见 tests/layer_guard.rs）                  │
├──────────────────────────────────────────────────────────────────────┤
│  能力层（core 直接接线的具体能力）                                     │
│  llm/chat.rs       Llm（complete / complete_structured / stream）      │
│  llm/embedding.rs  embed_text()（genai 阻塞 API，spawn_blocking）    │
│  channels/ChannelsPusher 主动外发（只读通道就绪注册表 + Config）      │
│  core/memory/Store   记忆读写（core 内具体类型，非独立能力口）          │
├──────────────────────────────────────────────────────────────────────┤
│  存储层：db/mod.rs（rusqlite + sqlite-vec）                            │
│  messages / memories / memory_vectors / sessions / channel_sessions / │
│  intent_state / self_profile / time_profiles / world_state / relations│
│  / meta                                                               │
│  （time_profiles / world_state / relations 表按 schema 保留、暂无代码   │
│   接线——aiworld 重设计后由新适配器写；self_profile 已由                 │
│   core/self_profile 读写，为人设数据源）                              │
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
  ├─ classify_inbound(sent_at, now) → 时效分级（§2.1）
  │  ├─ TooOld（>24h）→ Sessions::archive_only()：只按真实时刻入库留痕，
  │  │   不调 LLM、不回复、不产生 assistant 消息 → 本条结束
  │  └─ Fresh / Delayed → 继续下方流水线
  │
  ├─ IntentTracker::touch_user_reply(channel, id, session_id, sent_at)
  │  → 写穿 intent_state.last_user_reply_at = **消息真实时刻**（单调不后退）
  │    静默窗口从对方实际说话那一刻算起；用接收时刻会把窗口错误归零
  │
  ├─ Sessions::resolve("telegram", chat_id)
  │  → 内存通道映射命中即复用，未命中建映射（即写穿）
  │
  ├─ Sessions::add_message_at(session_id, "user", text, user_time)
  │  → 内存缓冲追加 + 即写穿 messages 表
  │  （路径：core/agent.rs，有通道时间戳时用之，否则记接收时刻）
  │
  ├─ ① 记忆召回（语义向量检索，≤ threshold 才入选）
  │     core::agent → core/context::fetch_recall → core/memory::Store::search
  │     （MEMORY_RECALL_LIMIT=5）
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
  │     context::system_prompt(profile, recall) → core/context/prompt.rs
  │       ├─ 角色设定：self_profile KV 表（AI 自我属性，中文原文注入，最高优先级；
  │       │   由 core/self_profile::Profile::block 渲染为「性格：…；人格：…」，
  │       │   对话/主动开口/延续判断三处共用同一 persona_block，空表整块省略）
  │       ├─ 英文行为指令：**身份 + 3 条变长示例**（短/长/短）。
  │       │   输出形状（换行、长度、句数）只由示例表达，不写禁令条目——
  │       │   模型对"不要用空行"这类禁令抑制力很弱，示例才是分布信号。
  │       │   规则只保留示例承载不了的价值判断：不出戏 / 不自称 AI / 身份边界。
  │       │   不设字数硬上限（真人不是"N 字以内"的人，硬截断会产生半句话）
  │       ├─ 召回记忆文本（"Below is what you know about the other person..."）
  │       └─ 延迟消息时追加 delay_notice() 时差说明（§2.1）——仅 Delayed 档
  │
  ├─ ③ 流式生成回复
  │     Llm::complete_stream(messages, on_delta)
  │       → genai_client: 按 LLM_PROVIDER 选 OpenAI / Gemini adapter
  │       → genai 0.6.5 流式 API
  │       → delta 逐段回调 on_delta（SSE 逐块推送 / CLI 实时打印）
  │       → max_tokens = STREAM_MAX_TOKENS(1024) 异常熔断：
  │         正常输出远小于此；触发即说明生成已跑偏，砍掉重来优于发出失控文本。
  │         **只兜底，不参与塑形**（输出形状由提示词示例负责）
  │
  ├─ ④ 回复入库，刷新会话时间戳
  │     Sessions::add_message(session_id, "assistant", reply) + touch_session(session_id)
  │     → 内存缓冲 + 即写穿
  │     → Agent::chat_stream 到此返回（带 session_id / reply / user_message_id）
  │
  └─ ⑤ 记忆沉淀（**回复交付之后**，宿主调 Agent::settle_memory）
  ├─ 判定：memory::user_memory_extraction::judge(text) → 极窄 JSON 标签，走 json_schema / json_object
  │     { is_memory, memory_type, tier, relation }（schema 用 enum 收窄）
  │     只做分类、不生成自然语言，输出维度小 → 模型几乎写不歪
  │     （relation 标签仍产出并随记忆提取保留，关系演化接线已随 aiworld 拆出，重设计后恢复）
         ├─ 事实化：仅当 is_memory=true 才追加一次调用
 │        user_memory_extraction::summarize(text) → 第三人称事实句（纯文本，不是 JSON）
         └─ 落库：Store::store(...) → memories 行 + memory_vectors（去重/分层/TTL）
               → llm::embedding::embed_text(content) → 向量化
               → 去重：与已有记忆距离 ≤ dedup_threshold → 只刷 updated_at，不新增
               → 插入 memories + memory_vectors
        任何环节失败只记 warn 并降级（不写记忆），绝不影响已交付的回复
```

### 2.1 离线积压消息的续传与时效分级

用户发完消息没等到回复就关机 / 断网 / 关掉 app，进程重启后这条消息仍在通道服务端
排队。**早期实现在启动时把拉取游标直接跳到最新，等于把这批消息整段丢弃**——用户
必须重发一遍，且"对方刚说过话"的时钟被改写（见 §4.2）。现在按两步处理：

**第一步：游标持久化，续传而非跳到最新。**

| 通道 | 游标 | 存取 |
|------|------|------|
| Telegram | `update_id`（服务端保留约 24h） | `meta` 表键 `channel_cursor:telegram` |
| iLink | `get_updates_buf` | `meta` 表键 `channel_cursor:ilink` |

- `db/channel.rs` 的 `get_cursor` / `set_cursor` 负责 SQL；
  `Sessions::load_cursor` / `save_cursor` 是 core 侧能力口（传输层不碰 db）。
- 游标在**整批确认后、处理消息之前**落盘：中途崩溃重启不会重放本批。
- 首次运行无游标 → Telegram 从 0 开始领取服务端保留的全部历史；这些消息几乎
  必然超时效，会被下面的 TooOld 分支挡掉，不产生任何回复。

**第二步：按消息真实时刻做时效分级。**

`core/session/sessions.rs` 的 `classify_inbound(sent_at, now)` 返回三档：

| 档位 | 时长 | 行为 |
|------|------|------|
| `Fresh` | ≤ 15 分钟 | 按实时消息正常回 |
| `Delayed` | 15 分钟 ~ 24 小时 | 正常回，但 system prompt 追加 `delay_notice()` 时差说明 |
| `TooOld` | > 24 小时 | `Sessions::archive_only()`：只按真实时刻入库留痕，不调 LLM、不回复 |

`delay_notice()` 明说"this was sent N hours ago, you're only just seeing it"，并禁止
模型假装刚收到、道歉、提 offline。这是**示例承载不了**的信息——模型看不到通道层时间
戳，只能显式告知。时钟回拨导致的负间隔按 0 处理（视作刚收到）。

> `MAX_REPLYABLE_HOURS = 24` 刻意与 Telegram 服务端的 update 保留窗口对齐：关机
> 超过 24h，那条消息服务端也已丢弃，任何实现都救不回来。

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
| `Agent::world_snapshot_text` / 世界心跳（`bin/server`） | 已删除 |
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

## 4. 主动陪伴引擎（core/intent/）

与世界引擎解耦，依赖 `INTENT_ENABLED=1`（默认关）且至少有一个通道启用。
**只在 `sa-link` 里 spawn**（`bin/sa-link.rs`）——`sa-web` 不跑这个循环，
`IntentTracker` 在 sa-web 中虽被装配但无消费者，只跑 Web UI 时不会有主动发言。

模块分工：`pace` 调度节奏 / `filter` 纯规则 / `speak` 开口决策 /
`continuation` 续话 / `sender` 派发 / `tracker` 状态簿。

### 4.1 主循环（core/intent/mod.rs）

```
intent::run(chat, session, tracker, pusher)   ← pusher = Arc<ChannelsPusher>
  loop {
      idle = pace::next_sleep()        ← 随机 45~179 分钟；安静时段则睡到结束 +30s
      sleep(idle)                       ← **先睡后跑**：启动后首个周期必然延迟一个间隔
      run_once(...)                     ← 每轮最多发出一条主动消息
  }
```

用的是 `tokio::time::sleep` 而非 `interval`，所以没有"首次 tick 立即触发"的语义。
这是刻意的：启动就开口会让用户觉得 bot 一睁眼就在聒噪。

### 4.2 单轮评估（run_once）

```
1. pace::in_quiet_hours() → 静默跳过（等待期间可能恰好跨入安静时段，竞态兜底）
2. tracker.eligible_candidates() → 内存状态簿 + filter::filter_eligible 过滤：
     - last_user_reply_at 必须存在（对方至少说过话）
     - 静默时长 ∈ [min_silence_hours, max_idle_days]（2h ~ 7d）
     - 距上次主动 ≥ intent_min_minutes
     - today_count < daily_limit（当日未超限）
3. 对每个候选（每轮最多 MAX_CANDIDATES_PER_CYCLE=5）：
     - sender::pushable(pusher, cand) → 通道可推性预筛
       （TG：长连通即 true；iLink：context_token 在 ILINK_CONTEXT_MAX_AGE_HOURS=12h
         窗口内且 sends_since_refresh < 10）
     - propose() → 最近 8 条历史 + 3 条记忆召回 → speak::decide 问 LLM（JSON）
     - sender::dispatch(...) → ChannelsPusher 外发，写回历史 + 计入配额
     - 发出即 return（一轮全通道合计一条）
```

**静默窗口的时钟**：`last_user_reply_at` 记的是**消息真实发出时刻**（通道报文
时间戳），不是接收时刻，见 §2.1。积压消息乱序到达时该值只前进不后退（SQL 层
`AND last_user_reply_at < ?1` + 内存侧同规则）。

`decide` 返回 `speak:false` 无退避计数、无失败升级，只按 `Decision.reason` 记
debug 日志；唯一的隐式退避是恒定随机间隔本身。

### 4.3 延续追加（continuation）

每次正常回复成功后（含主动回复），由 `continuation::after_reply()` 按
`INTENT_MAX_FOLLOWUPS` 限制的次数决定是否像真人一样接着补一两句（话题有延展性时，
或自己上一句话没说完）。追加句是"对话"而非"主动发起"，**不计入每日主动配额**、
不设 `last_intent_at`，但会写历史、且消耗 iLink 窗口次数。由各 relay 传入
`&ChannelsPusher` 复用同一能力口。

### 4.4 已知约束

| 约束 | 说明 |
|------|------|
| `INTENT_MIN_MINUTES` 一词两义 | 既是 `pace` 随机间隔下界，又是 `filter` 的同用户最小间隔。默认 45min 而实际调度 45~179min，故 §4.2 的间隔判断几乎不生效——节奏实际由随机间隔控制 |
| 沉默 > 7 天永久拉黑 | `INTENT_MAX_IDLE_DAYS`。主动发言**不**刷新 `last_user_reply_at`（只写 `last_intent_at` / `today_count`），时钟仅被用户入站推进；所以 bot 自己发满 8 条也救不回沉睡用户 |
| iLink 静默 > 12h 不可推 | `context_token` 过期，`pushable` 恒 false。比 7d 上限更早生效，协议限制无法绕过。iLink 实际可触发窗口只有约 10 小时 |
| 每日配额日界不一致 | `tracker` 内存用本地日、`filter` 判定用 UTC 日，东八区本地 00:00~08:00 期间配额会提前/延后重置 |

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
Store::search(query)
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

## 7. 数据库 Schema（v10）

SQLite + WAL 模式 + sqlite-vec 向量扩展。所有业务表仅做增量迁移，绝不 DROP。

| 表名 | 用途 |
|------|------|
| `meta` | 纯键值元信息（schema_version / embedding_dim / embedding_model / `channel_cursor:{channel}` 拉取游标，§2.1） |
| `sessions` | 会话（id / title / created_at / updated_at） |
| `messages` | 对话消息（role = user / assistant） |
| `memories` | 记忆事实（content / tier / expires_at / message_id） |
| `memory_vectors` | sqlite-vec0 向量表（memory_id / embedding float32[]） |
| `channel_sessions` | 通道 ↔ 会话映射（channel + external_id → session_id） |
| `intent_state` | 意图状态簿（今日计数 / 最后主动 / 最后用户回复时间） |
| `self_profile` | AI 自我属性 KV（key / value / updated_at），人设数据源 |
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
INTENT_ENABLED=0             # 总开关（默认关）
INTENT_MIN_MINUTES=45        # 随机间隔最小（兼作同用户主动最小间隔，见 §4.4）
INTENT_MAX_MINUTES=180       # 随机间隔最大
INTENT_QUIET_HOURS=23-7      # 安静时段（本机时间，HH-HH，跨午夜取 start > end；起止相同=禁用）
INTENT_DAILY_LIMIT=8         # 每用户每天上限
INTENT_MIN_SILENCE_HOURS=2   # 用户沉默多久才可能被主动联系
INTENT_MAX_IDLE_DAYS=7       # 超过多少天未回复则不再打扰
INTENT_MAX_FOLLOWUPS=3       # 每轮回复后最多连续追加几句话（0=关闭）
```

### 8.4 离线积压时效

入站消息时效分级的两个阈值是**编译期常量**（`core/session/sessions.rs`），暂不
开放为环境变量：`FRESH_WINDOW_MINUTES = 15`、`MAX_REPLYABLE_HOURS = 24`。
后者刻意与 Telegram 服务端的 update 保留窗口对齐（§2.1）。

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
9. **提示词靠示例不靠禁令**：输出形状（换行、长度、句数）只用 few-shot 示例表达，不写成"不要用空行""不超过 N 字"——模型对禁令的抑制力远弱于示例分布。规则只留示例承载不了的价值判断。字数硬上限会被砍成半句话，宁可不做；`max_tokens` 只作异常熔断。这是 `core/context/prompt.rs` 顶部维护约定的由来，也是防止提示词无限膨胀的纪律。
10. **入站按真实时刻记账**：消息时刻一律取通道报文时间戳，不取接收时刻。这同时决定两件事——消息入库的时间线、主动陪伴静默窗口的起点。离线积压按游标续传 + 时效分级处理（§2.1），不丢弃也不假装刚收到。
