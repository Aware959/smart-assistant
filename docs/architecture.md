# SMART Assistant 架构文档

> 本文描述后端 Rust 内核的分层结构、核心数据流与各子系统的协作方式。
> 
> 上一次更新：2026-10-05

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
│       │ Agent+Sessions │ Agent+Sessions                                │
│       │                │ + ChannelsPusher（主动外发唯一实现）        │
├───────┴───────────────┴──────────────────┴──────────────────────────┤
│  应用本体：core/（具体类型直连，能力以具体类型接线）                    │
│  app.rs      组合根 App：开 db、装配 Sessions / Llm / Store / Profile  │
│              → 注入 Agent。只装配不转发。                             │
│  agent.rs    Agent 单轮流水线 chat_stream()：工具循环内模型自行调      │
│              recall_memory / remember / send_message（§2）           │
│  session/    Sessions 会话内存运行时（首碰水合 + 变化即写穿）          │
│              + classify_inbound() 入站时效分级（§2.1）                │
│              + proactive_candidates() 主动候选派生（§4.2）             │
│  intent/     主动开口引擎：pace 调度 / filter 规则 / speak 决策 /     │
│              sender 派发（§4）                                         │
│  memory/     AI 记忆：Store（按需召回 + 模型主动写入，§6）             │
│  context/    上下文组装：历史时间线 / 记忆召回的取数与渲染             │
│  schema/     ChatInput/Output、ChatMessage/ChatCall、MemoryRecord/Hit、│
│              MessageTurn、IntentCandidate                               │
│  旧"存储端口"与 Host / ports.rs 窄端口已删除——core 与 db 直接接线，   │
│  会话运行时为本体 inherent；唯一反向引用是 ChannelsPusher（主动外发    │
│  属"外部能力"而非传输端点，见 tests/layer_guard.rs）                  │
├──────────────────────────────────────────────────────────────────────┤
│  能力层（core 直接接线的具体能力）                                     │
│  llm/chat.rs       Llm（complete / complete_structured / stream）      │
│                    stream 内做空白归一化（utils/textflow.rs，§2 ③）     │
│  llm/embedding.rs  embed_text()（genai 阻塞 API，spawn_blocking）    │
│  channels/ChannelsPusher 主动外发（只读通道就绪注册表 + Config）      │
│  core/memory/Store   记忆读写（core 内具体类型，非独立能力口）          │
├──────────────────────────────────────────────────────────────────────┤
│  存储层：db/mod.rs（rusqlite + sqlite-vec）                            │
│  messages / memories / memory_vectors / sessions / channel_sessions / │
│  self_profile / meta                                                  │
│  （self_profile 已由 core/self_profile 读写，为人设数据源；            │
│   time_profiles / world_state / relations 三张死表已由 v17 删掉——      │
│   代码从不读写，纯 schema 残留，删掉不丢信息）                         │
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
  ├─ Sessions::resolve("telegram", chat_id)
  │  → 内存通道映射命中即复用，未命中建映射（即写穿）
  │
  ├─ Sessions::add_message_at(session_id, "user", text, user_time)
  │  → 内存缓冲追加 + 即写穿 messages 表
  │  （路径：core/agent.rs，有通道时间戳时用之，否则记接收时刻）
  │
  ├─ ① 上下文组装（core::context，只读；与主动决策共用同一套实现）
  │     ⚠ **记忆不进提示词**——改由模型在工具循环里按需调 recall_memory（§6.3）
  │     context::fetch_history(conversation: &Sessions, session_id, history_count)
  │       ├─ 默认按"今天"取消息（日界 = 固定东八区 06:00；今日 <6 条自动并入昨天）
  │       │   （时区偏移此前由世界运行时推断，aiworld 拆出后暂用固定常量，待重设计）
  │       ├─ >60 条 → 保留头 8 条 + 尾 + 省略中间
  │       ├─ 内容保持原文，不带时间/角色前缀
  │       ├─ history_count 显式指定 → 退化为取最近 N 条
  │       └─ ⚠ 必须在"本条用户消息入库之前"取，否则本轮消息会被算进历史
  │     context::system_prompt(profile) → core/context/prompt.rs
  │       ├─ 角色设定：self_profile KV 表（AI 自我属性，中文原文注入，最高优先级；
  │       │   由 core/self_profile::Profile::block 渲染为「性格：…；人格：…」，
  │       │   对话与主动开口两处共用同一 persona_block，空表整块省略）
  │       ├─ 英文行为指令：**身份 + 3 条变长示例**（短/长/短）。
  │       │   输出形状（换行、长度、句数）只由示例表达，不写禁令条目——
  │       │   模型对"不要用空行"这类禁令抑制力很弱，示例才是分布信号。
  │       │   规则只保留示例承载不了的价值判断：不出戏 / 不自称 AI / 身份边界。
  │       │   不设字数硬上限（真人不是"N 字以内"的人，硬截断会产生半句话）
  │       │   ⚠ 示例块内**不得用空行分隔**：空行会被模型读成"AI 回复后留一段空白"
  │       │     的排版模板而照抄（实测 16% 回复被带出 \n\n，见 prompt.rs 约定 5）
│       └─ 延迟消息时追加 delay_notice() 时差说明（§2.1）——仅 Delayed 档
  │
  └─ ② 工具循环（core/agent.rs chat_stream）
        四个工具（llm/tool.rs 目录，每次请求都带，不做动态裁剪）：
          send_message  输出工具，唯一对外出口；一次调用发一条
          get_now       只读，当前本地时间
          recall_memory 只读，按需检索长期记忆（RECALL_LIMIT=5）
          remember      写入，沉淀一条长期记忆（§6.2）
        │
        每轮：Llm::complete_stream(messages, tool_calls, on_delta)
          → genai_client: 按 LLM_PROVIDER 选 OpenAI / Gemini adapter
          → genai 0.6.5 流式 API
          → 逐 chunk 过 ParagraphFlattener（utils/textflow.rs）→ delta 回调
            （SSE 逐块推送 / CLI 实时打印）。空白归一化必须在流里做：delta 是
            逐段下发的，事后改写全文会让"看到的"与"入库的"不一致
          → max_tokens = STREAM_MAX_TOKENS(2048) 异常熔断：
            **只兜底，不参与塑形**（输出形状由提示词示例负责）
        │
        分派：
          send_message(text)
            → 裁剪至 SEND_MESSAGE_MAX_CHARS=200 → on_message(text) 即时投递
            → Sessions::add_message(session_id, "assistant", text) 单独入库
              （**这条 assistant 消息同时是主动引擎的间隔账本**，见 §4.2）
          recall_memory(query)
            → core/memory::search(query, RECALL_LIMIT) → 语义向量检索
              （§6.3）；命中回填文本（含 [id] 供 remember.supersedes 指向）
          remember(content, kind, supersedes?)
            → Store::store(...)：supersedes 显式取代 / 相似度去重 / 新建
              → llm::embedding::embed_text(content) → 插入 memories + memory_vectors
              → kind 决定寿命（§6.1），强时间锚优先
        │
        **终止判据只有一条：本轮没有 tool_calls 即结束**
          （与 OpenAI Agents SDK 的 `text output and no tool calls`、
            Anthropic 的 `stop_reason != tool_use` 同构。曾评估 `finish` 工具
            做乐观终止，实测命中率仅 1/10，不足以承担判据）
        │
        任何工具失败只回填一次"没做到"让模型自行纠正，绝不冒泡成对话失败；
        记忆链路的失败姿态就是"这条没记住"，不会连累这轮回复
```

> 模型不直接写正文：想说的话必须调 `send_message`，因此"一句一句回"是模型的
> 自然行为而非提示词雕琢出来的。记忆写入也在同一个循环里（`remember`），没有
> 回复交付后的旁路判定——模型看得见整段对话，"值不值得记、指的是哪件旧事"只有
> 它能判断。

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
| `memory/current_relation.rs`（亲密度 × 信任度 + `apply_event` 演化） | 已删除（见下方说明） |
| `memory/current_phase.rs`（时段推断 `infer_from_hours`） | 已删除（见下方说明） |
| 表 `world_state` / `relations` / `time_profiles` | 已由 v17 删掉的死表（代码从不读写，纯 schema 残留，删掉不丢信息） |

> `current_relation` / `current_phase` 一并删除的理由：两者都从未接线到任何生产路径，
> 而语义与记忆域重叠——"亲密度 / 信任度"表达的是"对方是谁、该被怎么对待"，本来
> 就该记成 `MemoryKind::Trait`，重复一份结构化副本只会漂移。现在需要"关系感"时走
> 记忆（trait / preference）与人设（`self_profile`）。

> 附带一处既有决策：此前的"今日叙事"（narrative）随世界一并移除——它曾因
> `updated_at` 每跳刷新而被 120 分钟间隔门永久卡死，实际从未生效，其列仅按
> schema 只增保留。
>
> 历史上下文"今天"日界的时区偏移原由世界运行时按画像动态推断，现改为固定东八区
> 常量（`core/context/history.rs` 的 `USER_OFFSET_MINUTES`），重设计后恢复动态推断。
>
> 重新设计定稿后：端口契约、内存适配器与写穿回归 `runtime`；关系感不再另立结构化
> 副本，直接复用记忆域的 trait / preference 与 `self_profile`。

---

## 4. 主动开口引擎（core/intent/）

与世界引擎解耦，依赖 `INTENT_ENABLED=1`（默认关）且至少有一个通道启用。
**只在 `sa-link` 里 spawn**（`bin/sa-link.rs`）——`sa-web` 不跑这个循环，
只跑 Web UI 时不会有主动发言。

模块分工：`pace` 调度节奏 / `filter` 纯规则 / `speak` 开口决策 / `sender` 派发。

### 4.1 主循环（core/intent/mod.rs）

```
intent::run(chat, session, pusher)   ← pusher = Arc<ChannelsPusher>
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
2. Sessions::proactive_candidates(now) → db/channel::list_proactive_rows 派生 + filter 过滤：
     - 候选人来自 channel_sessions（有会话映射即候选），时间账本取自 messages：
       last_user_at = MAX(created_at) WHERE role='user'
       last_assistant_at = MAX(created_at) WHERE role='assistant'
     - last_user_at 必须存在（对方至少说过话）
     - 静默时长 ∈ [min_silence_hours, max_idle_days]（2h ~ 7d）
     - 距最近一条 assistant 消息 ≥ intent_min_minutes（不区分回复/主动开口）
     - **无每日配额**
3. 对每个候选（每轮最多 MAX_CANDIDATES_PER_CYCLE=5）：
     - sender::pushable(pusher, cand) → 通道可推性预筛
       （TG：长连通即 true；iLink：context_token 在 ILINK_CONTEXT_MAX_AGE_HOURS=12h
         窗口内且 sends_since_refresh < 10）
     - propose() → 最近 8 条历史 + 3 条记忆召回 → speak::decide 问 LLM（JSON）
     - sender::dispatch(...) → ChannelsPusher 外发 + 写回 assistant 历史
     - 发出即 return（一轮全通道合计一条）
```

**候选状态不落独立状态表**：原先的 `intent_state` 表存的每个值都能从 `messages`
时间线算出来，等于把已有事实抄一份——抄写路径一旦漏掉（换通道、删会话、重放）就与
真实时间线漂移。v13 已删掉该表（§7）。副作用是状态天然随进程重启存活。

**静默窗口的时钟**：`last_user_at` 取 `messages.created_at` 的最大值，而该列按通道
报文时间戳写入（**消息真实发出时刻**，非接收时刻，见 §2.1），因此离线积压消息不会把
窗口错误归零；乱序到达时 `MAX` 天然只取最新。

**防连环轰炸**：`last_assistant_at` 不区分"回复"与"主动开口"——只要最近一次说话距今
不足 `intent_min_minutes` 就不开口。这条门槛不需要额外状态：发出成功后写回一条
`assistant` 历史（`sender::dispatch`）即完成记账。

`decide` 返回 `speak:false` 无退避计数、无失败升级，只按 `Decision.reason` 记
debug 日志；唯一的隐式退避是恒定随机间隔本身。

### 4.3 已知约束

| 约束 | 说明 |
|------|------|
| `INTENT_MIN_MINUTES` 一词两义 | 既是 `pace` 随机间隔下界，又是 `filter` 的同用户最小间隔。默认 45min 而实际调度 45~179min，故 §4.2 的间隔判断几乎不生效——节奏实际由随机间隔控制 |
| 沉默 > 7 天永久拉黑 | `INTENT_MAX_IDLE_DAYS`。该时钟只被用户入站推进（主动发言写的是 `assistant` 消息，不推进用户侧时钟），所以 bot 自己主动满 7 条也救不回沉睡用户 |
| iLink 静默 > 12h 不可推 | `context_token` 过期，`pushable` 恒 false。比 7d 上限更早生效，协议限制无法绕过。iLink 实际可触发窗口只有约 10 小时 |

---

## 5. 时间世界——已随 aiworld 拆出

原 `World::observe()`（消息入库后重算作息画像）与 `time_profiles` 写穿随 aiworld
整体移除。`memory/current_phase.rs` 的时段推断纯逻辑一并删除，重设计时若再做作息
画像由新适配器（喂数、水合、写穿）重新接线。

按天上下文构建（`core/context/history.rs` 的 `build_day_history`）的关键常量：

| 常量 | 值 | 含义 |
|------|----|------|
| `DAY_START_HOUR` | 6 | 当地 06:00 为一日起点（当前按固定东八区偏移） |
| `DAY_POOL_THRESHOLD` | 6 | 今日 <6 条时，自动并入昨天 |
| `DAY_CONTEXT_MAX` | 60 | 超出此条数时压缩 |
| `DAY_KEEP_HEAD` | 8 | 压缩时保留的头部条数 |

---

## 6. 记忆系统（core/memory/）

### 6.1 种类白名单与寿命

进库的门槛是一份**可枚举的白名单** `MemoryKind`——像 AI 人设抽成 `self_profile`
KV 表一样，把"什么值得记"从散文变成四项各有名字、各带规则的清单：

| kind | 含义 | 默认寿命 |
|------|------|----------|
| `trait` | 对方是谁、该被怎么对待 | 永不过期 |
| `preference` | 偏好、习惯、称呼、约定 | 永不过期 |
| `routine` | 作息、周期性安排 | 90 天 |
| `plan` | 进行中的事、待办意向 | 30 天 |

`trait` / `preference` 刻意不给期限：过期只会让 AI 在对方没变的情况下失忆。

寿命由代码算，不由模型给（`resolve_lifetime`）：内容里有强时间锚（"下周三"）时按锚
算精确到期日，否则回落到种类的默认窗口。schema 层用 CHECK 约束收窄 `memory_type`，
杜绝自由文本混进来。

**"这一条够不够格"不由本模块判定**——那需要上下文，而判定过去被放在只有单条消息的
旁路里，只能看见"对方自称只是傲娇"这种半截话。现在写入由模型在工具循环里发起
（`llm::tool` 的 `remember`）：它看得见前后文，能自己消解指代，也知道值不值得留。

### 6.2 写入：模型主动 remember

```
remember(content, kind, supersedes?)
  │
  1. supersedes 显式取代 → 改写那一行（最优先，只有模型明确指出才改）
  2. 相似度去重 → 与库中某条余弦相似度 ≥ memory_dedup_min_similarity → 改写
  3. 否则新建 → memories 行 + memory_vectors（embed → 插入）
```

### 6.3 向量检索（按需召回）

记忆**不进提示词**——改由模型在工具循环里调 `recall_memory(query)`。好处是每轮不
再白跑一次 embedding + 向量检索，也不再每轮灌入 5 条大概率无关的记忆。风险是模型
可能忘记调工具，由工具描述里的「不确定用户相关信息时先查」兜住。

```
search(query)
  │
  ├─ db.vec_search_usable() → 向量签名不匹配时静默降级为空（等 sa-mem rebuild）
  ├─ llm::embedding::embed_text(query) → EMBEDDING_API_URL（同步阻塞）
  └─ db::memory::search_similar(vector, limit)
       SELECT ... FROM memory_vectors
       WHERE embedding MATCH ?1 AND k = ?3    ← vec0 kNN 查询，必须 k = ?
       ORDER BY distance
       → 过滤余弦相似度 ≥ memory_recall_min_similarity，且 expires_at 未到期
```

---

## 7. 数据库 Schema（v17）

SQLite + WAL 模式 + sqlite-vec 向量扩展。

**当前 schema 是单一真源**：完全由 `db/schema.rs::init_schema` 里的 `CREATE TABLE IF NOT EXISTS` 定义，新库打开时直接建最新结构。**历史逐版本迁移（v6~v16）已压平**——单人单库且真实库已升到最新，保留逐版本补丁只会增加 `schema.rs` 体积、无实际收益。压平后旧版本库不再支持平滑升级，需 `rm` 后重建。

| 表名 | 用途 |
|------|------|
| `meta` | 纯键值元信息（schema_version / embedding_dim / embedding_model / `channel_cursor:{channel}` 拉取游标，§2.1） |
| `sessions` | 会话（id / title / created_at / updated_at） |
| `messages` | 对话消息（role = user / assistant；工具结果行用 `role='assistant'` + `tool_call_id`/`tool_name` 标记承载，避免改 role CHECK；created_at 按通道真实发出时刻写） |
| `memories` | 记忆事实（content / memory_type[CHECK 枚举] / expires_at / message_id） |
| `memory_vectors` | sqlite-vec0 向量表（memory_id / embedding float32[]） |
| `channel_sessions` | 通道 ↔ 会话映射（channel + external_id → session_id）；主动候选的来源表 |
| `self_profile` | AI 自我属性 KV（key / value / updated_at），人设数据源 |

唯一的迁移例外（与"默认值都能重算出来"同一条判据，删表不丢信息）：

| 版本 | 动作 | 理由 |
|------|------|------|
| v17 | `DROP TABLE relations` / `world_state` / `time_profiles` | 三张表由 v7/v8 引入（关系 / 世界状态 / 作息画像），代码从不读写（无 DAO、无业务调用点），纯 schema 残留。与早先 v13 删 `intent_state` 同理——它们存的每个值都不被任何逻辑当真值来源，删掉不丢信息。按约定 DROP 业务表需升版本号，故从 16 升到 17 |

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
```

- `openai` 模式：genai `OpenAIAdapter`（兼容 Groq / DeepSeek / LM Studio 等）
- `google` 模式：genai `GeminiAdapter`（支持 JsonSpec 结构化 + 流式）

### 8.2 通道开关

```env
TELEGRAM_ENABLED=1   # 缺省开启；0 = 明确不启动（仍需 token 非空）
ILINK_ENABLED=0      # 0/留空 = 不启动；1 = 启动（有 token 直连，无 token 扫码）
```

### 8.3 主动开口

```env
INTENT_ENABLED=0             # 总开关（默认关）
INTENT_MIN_MINUTES=45        # 随机间隔最小（兼作距上次 assistant 发言的最小间隔，见 §4.3）
INTENT_MAX_MINUTES=180       # 随机间隔最大
INTENT_QUIET_HOURS=23-7      # 安静时段（本机时间，HH-HH，跨午夜取 start > end；起止相同=禁用）
INTENT_MIN_SILENCE_HOURS=2   # 用户沉默多久才可能被主动联系
INTENT_MAX_IDLE_DAYS=7       # 超过多少天未回复则不再打扰
```

这些全是**机械门槛**：是否真的开口由 LLM 看对话自行判断，因此不注入提示词。
`INTENT_DAILY_LIMIT`（每日上限）与 `INTENT_MAX_FOLLOWUPS`（延续条数）已随
`intent_state` 表与 continuation 模块一并移除。

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
4. **Schema 迁移**：当前 schema 完全由 `init_schema` 的 `CREATE TABLE IF NOT EXISTS` 定义（单一真源），新库直接建最新结构。历史逐版本迁移（v6~v16）已压平到 v17 基线——单人单库、真实库已升最新，保留逐版本补丁只膨胀 `schema.rs`。压平后旧版本库不再平滑升级，需 `rm` 后重建。仅保留一条 DROP 例外（v17 删三张死表），判据仍是同一条——**要删的东西必须能从别处重算出来 / 不被任何逻辑当真值来源**（见 §7）。
5. **向量签名锁定**：embedding 模型变更不会自动重建，由独立工具 `cargo run --bin sa-mem -- rebuild` 手动触发，保证旧库不丢数据。
6. **vec0 kNN 约束**：查询必须写 `k = ?`，把 LIMIT 写在外层查询会报 "A LIMIT or 'k = ?' constraint is required"。
7. **JSON 结构化输出**：开口决策走 json_schema（支持的模型自动约束输出格式，不支持时退化为自由 JSON + 容错解析）。模型输出一律当**不可信输入**：字段缺失 / `null` / 类型不符 / 空白都回退默认值，不让一次格式抖动变成业务失败。记忆写入改由工具调用承载（参数有 schema 约束），不再走独立判定。
8. **输出与写入都由模型自己发起**：模型想说的话必须调 `send_message`（唯一对外出口），想记的事必须调 `remember`。记忆不再挂在对话路径之外做旁路判定——过去每条消息都要跑一次"这条值得记吗"，而判定者只看得到单条消息，写出一堆悬空指代。现在模型在同一循环里既看得见整段对话、又能直接落笔，`remember` 的三道闸写在**工具描述**里（每轮都在上下文，且"怎么用"与"什么时候用"同处一地）。
9. **记忆全链路降级**：`recall_memory` / `remember` 失败只回填一次"没做到"让模型自行纠正，绝不冒泡成"对话失败"。记忆的失败姿态本就是"这条没记住"，不该连累这轮回复。
10. **提示词塑形 + 流内归一化，两层叠加**：输出形状（换行、长度、句数）主要由 few-shot 示例表达，不写成"不要用空行""不超过 N 字"——模型对禁令的抑制力远弱于示例分布。规则只留示例承载不了的价值判断。字数硬上限会被砍成半句话，宁可不做；`max_tokens` 只作异常熔断。提示词是概率约束，兜不住全部——实测 DeepSeek-V4.1-Flash 仍有 16% 回复带 `\n\n`，故流内再用 `utils/textflow.rs` 归一化空白（收敛连续空白、裁首尾、CJK 两侧不补空格）。**归一化 ≠ 塑形**：它只动空白、不截断不删词，所以不产生半句话，与被砍掉的 `MAX_REPLY_CHARS` 有本质区别。归一化必须发生在 `on_delta` 之前，事后改写全文会让"看到的"和"入库的"不一致。`core/context/prompt.rs` 顶部的维护约定（含"示例块内不得用空行分隔"）是防提示词膨胀与自相矛盾的纪律。
11. **入站按真实时刻记账**：消息时刻一律取通道报文时间戳，不取接收时刻。这同时决定两件事——消息入库的时间线、主动开口静默窗口的起点。离线积压按游标续传 + 时效分级处理（§2.1），不丢弃也不假装刚收到。
12. **能不存的就不存**：删状态前先问"这些值能不能从别处算出来"。`intent_state` 的每个字段都是 `messages` 时间线的一次 `MAX`，抄一份就多一条会漂移的写穿路径（v13 已删，迁移已压平，见 §7）；`memories.tier` 与 `memory_type` + `expires_at` 表达同一件事，留着只会让前端拿到自相矛盾的两组值。状态表是缓存，能重算就别当真值来源——这条判据也正是 v17 删三张死表的依据。
