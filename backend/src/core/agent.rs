//! 编排内核（Agent）：把会话 + 能力组件组装成可复用的业务流水线。
//!
//! 重构后会话是 core 内具体类型（[`Sessions`]），能力直接以具体类型接线：
//! LLM = [`crate::llm::chat::Llm`]，记忆 = [`crate::core::memory::Store`]。

use std::sync::Arc;

use chrono::Datelike;

use crate::core::context::{self, AgentContext};
use crate::core::memory::{MemoryKind, MemoryRecord, Store};
use crate::core::schema::message::ChatMessage;
use crate::core::self_profile::Profile;
use crate::core::session::Sessions;
use crate::error::Result;
use crate::llm::chat::Llm;

/// 对话输入：处理层拿到的最低输入契约（会话 / 消息 / 可选的显式历史）。
#[derive(Debug, Clone)]
pub struct ChatInput {
    pub message: String,
    /// 会话 id。留空时后端自动新建会话并返回新 id。
    pub session_id: Option<String>,
    /// 显式携带的历史轮次（可选，补充上下文）；留空时后端自动从会话消息构建。
    pub history: Vec<ChatMessage>,
    /// 自动构建上下文的方式：缺省按"今天"取消息（日界=用户当地 06:00、带时间戳）；
    /// 显式指定 N 时改为取最近 N 条纯文本。
    pub history_count: Option<u32>,
    /// 用户端消息的真实发出时刻（RFC3339 世界时，来自通道报文时间戳）。
    /// 缺省时为后端接收时刻；用于消息记录的真实时间线。
    pub user_time: Option<String>,
}

/// 对话输出：一轮对话的结果（会话 / 回复 / 来源消息 / 本轮写入的记忆）。
#[derive(Debug, Clone)]
pub struct ChatOutput {
    pub session_id: String,
    /// 本轮全部消息以换行拼接。给只需要单一文本的宿主（CLI、兼容旧协议）。
    pub reply: String,
    /// 本轮投递的**每一条独立消息**，顺序即投递顺序。
    ///
    /// 通道必须按这个列表逐条发送——把 `reply` 整段发出去等于退回单条长回复，
    /// 整个"一句一句回"就白改了。`reply` 只是它的便捷视图。
    pub messages: Vec<String>,
    /// 本条用户消息的 id（记忆的来源消息 id 即指向它）。
    pub user_message_id: String,
    /// 本轮写入的记忆，取**最后一条**。
    ///
    /// 模型一轮里可能写不止一条（写了新事实又改写了旧的），而这里只带一条出来，
    /// 所以取最后写的——那通常是它对当前话题做完的最后一次修正。记忆库才是真源，
    /// 这个字段只用于让宿主展示"我记住了什么"。
    pub memory: Option<MemoryRecord>,
}

/// Agent：完成单轮对话 / 记忆落库等核心业务流程。
pub struct Agent {
    conversation: Arc<Sessions>,
    llm: Arc<Llm>,
    memory: Arc<Store>,
    profile: Arc<Profile>,
}

impl Agent {
    /// 用具体会话适配器与两个能力组件构造内核。
    pub fn new(
        conversation: Arc<Sessions>,
        llm: Arc<Llm>,
        memory: Arc<Store>,
        profile: Arc<Profile>,
    ) -> Self {
        Self {
            conversation,
            llm,
            memory,
            profile,
        }
    }

    /// 暴露 LLM 能力（主动陪伴等周边编排复用同一具体实现）。
    pub(crate) fn chat_llm(&self) -> Arc<Llm> {
        self.llm.clone()
    }

    /// 自我属性人设区块文本（主动决策 / 延续判断复用同一人设数据源）。
    pub(crate) fn profile_block(&self) -> String {
        self.profile.block()
    }

    /// 最近 `limit` 条对话的渲染文本（延续判断等只需历史的场景，避免白跑一次召回）。
    pub(crate) fn recent_history_text(&self, session_id: &str, limit: usize) -> String {
        context::recent_history_text(self.conversation.as_ref(), session_id, limit)
    }

    /// 记忆召回的纯文本渲染（主动决策阅读用，失败静默为空）。
    pub(crate) fn recall_text(&self, query: &str, limit: usize) -> String {
        match context::fetch_recall(self.memory.as_ref(), query, limit) {
            Ok(hits) => context::plain_lines(&hits),
            Err(_) => String::new(),
        }
    }

    /// 对话主入口（工具循环 + 流式投递）。
    ///
    /// 模型不再直接写正文：它想说的话必须调用 `send_message` 工具，一次调用一条，
    /// 因此"一句一句回"是模型的自然行为而不是提示词雕琢出来的。
    ///
    /// 循环判据只有一条：**本轮没有 `tool_calls` 即结束**。与 OpenAI Agents SDK
    /// （`text output and no tool calls` → final output）和 Anthropic
    /// （`stop_reason != "tool_use"`）同构。曾评估 `finish` 工具做乐观终止，
    /// 实测 `DeepSeek-V4.1-Flash` 命中率仅 1/10，不足以承担判据，故不引入。
    ///
    /// 流程：
    /// 1. 取历史（须在用户消息入库前取）→ 用户消息入库；
    /// 2. 组装 messages（**不再无条件召回记忆**，改由模型按需调 `recall_memory`）；
    /// 3. 循环：流式生成 → 有工具则执行并回填、无工具则结束；
    /// 4. 每条 `send_message` 的文本即时回调 `on_message` 并单独入库。
    ///
    /// **记忆写入也在这个循环里**（`remember` 工具），不再有回复交付后的旁路步骤。
    /// 模型看得见整段对话，所以"这句话值不值得记、指的是哪件旧事"只有它能判断；
    /// 而写入失败也只是回填一次"没记住"，不会连累这轮回复——这本来就是记忆该有的
    /// 失败姿态，只是过去它挂在对话主流程之外、每条消息都要跑一遍判定。
    pub fn chat_stream<F>(&self, input: &ChatInput, mut on_message: F) -> Result<ChatOutput>
    where
        F: FnMut(&str) + Send,
    {
        // 追踪：一次调用 = 一个 trace_id span，本函数内所有 target="trace" 的事件
        // 都会带上 trace_id / session_id，便于把"一次请求的各环节"归成一组。
        let span = crate::utils::trace::turn_span();
        let _enter = span.enter();
        let turn_started = std::time::Instant::now();

        // 空消息不入库、不检索，直接返回。
        if input.message.trim().is_empty() {
            return Ok(ChatOutput {
                session_id: input.session_id.clone().unwrap_or_default(),
                reply: String::new(),
                messages: Vec::new(),
                user_message_id: String::new(),
                memory: None,
            });
        }

        // 1. 会话：复用传入的 id；不存在或未传则自动新建。
        let session = self
            .conversation
            .get_or_create_session(input.session_id.as_deref())?;

        // 首条消息自动生成会话标题（截取前 24 个字符）。
        if session.title.trim().is_empty() {
            let title: String = input.message.chars().take(24).collect();
            self.conversation.set_session_title(&session.id, &title)?;
        }
        span.record("session_id", session.id.as_str());
        tracing::info!(
            target: "trace",
            stage = "start",
            history_count = input.history_count,
            user_time = input.user_time.as_deref().unwrap_or(""),
            message = %crate::utils::trace::Text(&input.message),
            "对话开始"
        );

        // 2. 上下文（一）：历史消息。**必须在"本条用户消息入库"之前取**，否则本轮消息
        // 会被算进历史重复出现。默认按"今天"取对话（日界=用户当地 06:00，今日过少自动
        // 并入昨天，让 AI 有连续性）；显式 history_count 则退化为取最近 N 条。
        let history_turns =
            context::fetch_history(self.conversation.as_ref(), &session.id, input.history_count)?;
        tracing::info!(
            target: "trace",
            stage = "history",
            turns = history_turns.len(),
            history = %crate::utils::trace::Lazy(|| render_messages(&history_turns)),
            "历史消息已取"
        );

        // 用户消息入库（记录）。带通道报文时间戳时用之（消息记录精确时间线），否则记接收时刻。
        let user_message = match input.user_time.as_deref().and_then(parse_rfc3339) {
            Some(at) => {
                self.conversation
                    .add_message_at(&session.id, "user", &input.message, at)?
            }
            None => self
                .conversation
                .add_message(&session.id, "user", &input.message)?,
        };

        // 3. 上下文（二）：组装 messages。
        //
        // **记忆不进提示词**——改成模型按需调 `recall_memory`，好处是每轮不再白跑一次
        // embedding + 向量检索，也不再每轮灌入 5 条大概率无关的记忆（实测平均 4.7 条
        // 里约 3.3 条是噪音）。代价是模型可能忘记调工具，这个风险由工具描述里的
        // 「不确定用户相关信息时先查」兜住。
        let ctx = AgentContext {
            history: history_turns,
            memories: Vec::new(),
        };
        tracing::info!(session = %session.id, "对话阶段：提示词组装完成");
        let profile = self.profile_block();
        // 延迟消息（离线积压）才追加时差说明，让模型知道"这条是几小时前发的"。
        let system = match delay_notice(input.user_time.as_deref(), chrono::Utc::now()) {
            Some(notice) => format!("{}{notice}", context::system_prompt(&profile)),
            None => context::system_prompt(&profile),
        };
        let mut messages = vec![ChatMessage::system(system)];
        for turn in ctx.history.iter().chain(input.history.iter()) {
            messages.push(ChatMessage::new(turn.role.clone(), turn.content.clone()));
        }
        messages.push(ChatMessage::user(input.message.clone()));
        tracing::info!(
            target: "trace",
            stage = "prompt",
            n = messages.len(),
            messages = %crate::utils::trace::Lazy(|| render_messages(&messages)),
            "提示词与消息已组装"
        );

        // 4. 工具循环。
        let tools = crate::llm::tool::definitions();
        // 写入过的记忆留给 ChatOutput 回报宿主；工具上下文是共享引用，
        // 所以这里用内部可变性收集，不能靠 &mut。
        let tool_ctx = TurnTools {
            memory: self.memory.as_ref(),
            user_message_id: user_message.id.as_str(),
            written: std::cell::RefCell::new(Vec::new()),
        };
        let llm_phase = std::time::Instant::now();
        let mut delivered: Vec<String> = Vec::new();
        // 连续多少轮只调只读工具、一次都没说话。防止模型在工具上来回打转不说话。
        let mut mute_rounds = 0u32;

        for iter in 1..=MAX_TOOL_ITER {
            let session_id = session.id.clone();
            // 直接正文**不在这里下发**：模型绕过工具写正文时，`complete_stream_with_tools`
            // 只在确认整轮无工具调用后才回调一次，而那一次的正文由下面的 `emit` 统一
            // 投递+入库。若把它也接到 `on_message`，同一条正文会被下发两次（这里一次、
            // emit 一次），而这里这条还没进 `delivered`、也没入库。
            let mut on_delta = |_piece: &str| {};
            let outcome =
                match self
                    .llm
                    .complete_stream_with_tools(&messages, &tools, &mut on_delta)
                {
                    Ok(o) => o,
                    Err(e) => {
                        // 最后一轮失败且一条都没发出去才算失败；已经发过消息就按已发送的收尾。
                        if delivered.is_empty() {
                            return Err(e);
                        }
                        tracing::warn!(
                            session = %session_id,
                            iter,
                            error = %e,
                            delivered = delivered.len(),
                            "工具循环中断，按已投递消息收尾"
                        );
                        break;
                    }
                };

            // 终止判据：本轮没有任何工具调用。
            if !outcome.has_tool_calls() {
                if !outcome.text.trim().is_empty() {
                    emit(
                        &mut on_message,
                        &mut delivered,
                        self.conversation.as_ref(),
                        &session_id,
                        &outcome.text,
                    )?;
                }
                tracing::info!(
                    target: "trace",
                    stage = "loop_end",
                    iter,
                    reason = "no_tool_calls",
                    delivered = delivered.len(),
                    "工具循环结束"
                );
                break;
            }

            // 压回 assistant 轮（含本轮全部工具调用）。
            tracing::info!(
                session = %session_id,
                iter,
                tool_calls = outcome.tool_calls.len(),
                reasoning_chars = outcome.reasoning_chars,
                direct_text_chars = outcome.text.chars().count(),
                ?outcome.stop_reason,
                "本轮模型产出"
            );
            messages.push(ChatMessage::assistant_calls(
                outcome.text.clone(),
                outcome.tool_calls.clone(),
            ));

            let mut spoke = false;
            for call in &outcome.tool_calls {
                // 工具入参逐条落日志：出问题第一反应就是"模型到底调了什么、传了什么"，
                // 而参数正文只存在于工具调用里——不记就永远无法事后核对。
                // 用 debug 承载完整入参、info 只带工具名与条数，避免正常对话刷屏。
                tracing::info!(
                    session = %session_id,
                    iter,
                    tool = %call.fn_name,
                    tools = outcome.tool_calls.len(),
                    "执行工具调用"
                );
                tracing::debug!(
                    session = %session_id,
                    tool = %call.fn_name,
                    call_id = %call.call_id,
                    arguments = %call.fn_arguments,
                    "工具调用入参"
                );
                match crate::llm::tool::dispatch(call, &tool_ctx) {
                    crate::llm::tool::ToolOutcome::Sent(text) => {
                        // 已投递的正文另记一条：这是用户实际看到的内容，
                        // 与"入参"分开记，便于核对投递前是否被归一化改写。
                        tracing::info!(
                            session = %session_id,
                            chars = text.chars().count(),
                            "send_message 已投递"
                        );
                        emit(
                            &mut on_message,
                            &mut delivered,
                            self.conversation.as_ref(),
                            &session_id,
                            &text,
                        )?;
                        spoke = true;
                        messages.push(ChatMessage::tool_result(
                            call,
                            crate::llm::tool::SEND_MESSAGE_ACK,
                        ));
                    }
                    crate::llm::tool::ToolOutcome::Filled(text) => {
                        messages.push(ChatMessage::tool_result(call, text));
                    }
                    crate::llm::tool::ToolOutcome::Rejected(reason) => {
                        tracing::warn!(
                            session = %session_id,
                            tool = %call.fn_name,
                            reason = %reason,
                            "工具调用被拒绝"
                        );
                        messages.push(ChatMessage::tool_result(
                            call,
                            format!("调用失败：{reason}"),
                        ));
                    }
                }
            }

            mute_rounds = if spoke { 0 } else { mute_rounds + 1 };
            tracing::info!(
                target: "trace",
                stage = "loop_step",
                iter,
                calls = outcome.tool_calls.len(),
                spoke,
                mute_rounds,
                reasoning_chars = outcome.reasoning_chars,
                stop = ?outcome.stop_reason,
                delivered = delivered.len(),
                "工具循环单步完成"
            );

            if mute_rounds >= MAX_MUTE_ROUNDS {
                tracing::warn!(
                    session = %session_id,
                    iter,
                    "连续多轮未产出消息，提前结束工具循环"
                );
                break;
            }
            if iter == MAX_TOOL_ITER {
                tracing::warn!(
                    session = %session_id,
                    "工具循环达到迭代上限，强制收尾"
                );
            }
        }

        let llm_ms = llm_phase.elapsed().as_millis() as u64;
        tracing::info!(session = %session.id, llm_ms, "对话阶段：工具循环完成");

        if delivered.is_empty() {
            return Err(crate::error::SqlError::Config(
                "模型未产出任何可发送的消息".to_string(),
            ));
        }
        let reply = delivered.join("\n");

        self.conversation.touch_session(&session.id)?;

        span.record("user_message_id", user_message.id.as_str());
        tracing::info!(
            target: "trace",
            stage = "done",
            user_message_id = %user_message.id,
            messages = delivered.len(),
            total_ms = turn_started.elapsed().as_millis() as u64,
            "对话完成"
        );

        // 本轮写入的最后一条记忆（若有）。取在 return 之前，
        // 让借用先结束——否则 ChatOutput 会带着 Ref 活到函数尾。
        let last_memory = tool_ctx.written.borrow().last().cloned();

        Ok(ChatOutput {
            session_id: session.id,
            reply,
            messages: delivered,
            user_message_id: user_message.id,
            memory: last_memory,
        })
    }
}

/// 工具循环的迭代硬上限。
///
/// 迭代**不是**消息条数——模型会在单次响应里并行吐出多条 `send_message`
/// （实测一次给 3 条、一次给 22 条都出现过）。一轮迭代通常对应"取一次上下文 /
/// 说一句话"，`recall_memory` + `send_message` 最多两三轮，8 已有大量余量。
/// 市场默认值参考：Vercel AI SDK `stepCountIs(20)`、OpenAI `max_turns`。
const MAX_TOOL_ITER: usize = 8;

/// 连续多少轮只调只读工具、一句话都没说就收尾。
///
/// 防的是模型在 `get_now` / `recall_memory` 上来回打转却不说人话。只读工具
/// 没有副作用，所以这里直接结束比继续等更划算。
const MAX_MUTE_ROUNDS: u32 = 2;

/// 一轮对话内的工具执行环境。
///
/// 只读工具与 `remember` 共用这一个对象：`remember` 要写状态，所以这里带上了本轮
/// 用户消息 id（作为记忆的来源）；只读工具用不到它，但让两条路径共用一份上下文
/// 比拆成两个对象少一层分发。
struct TurnTools<'a> {
    memory: &'a Store,
    /// 本条用户消息 id：记忆的 `message_id` 指向它。
    user_message_id: &'a str,
    /// 本轮写入的记忆，最后一条随 `ChatOutput` 回报宿主。
    written: std::cell::RefCell<Vec<MemoryRecord>>,
}

impl crate::llm::tool::ToolContext for TurnTools<'_> {
    /// 当地时间。刻意复用历史日界的同一偏移常量——"今天"的起点和"现在几点"
    /// 必须用同一个时区，否则会出现"今天还没开始但已经下午三点"的矛盾。
    fn now(&self) -> String {
        let offset = context::USER_OFFSET_MINUTES;
        let local = chrono::Utc::now() + chrono::Duration::minutes(offset);
        const WEEKDAYS: [&str; 7] = [
            "星期一",
            "星期二",
            "星期三",
            "星期四",
            "星期五",
            "星期六",
            "星期日",
        ];
        let weekday = WEEKDAYS[local.weekday().num_days_from_monday() as usize];
        let hour = local.format("%H:%M").to_string();
        format!(
            "{} {} {weekday} (UTC{:+03}:{:02})",
            local.format("%Y-%m-%d"),
            hour,
            offset / 60,
            offset.abs() % 60
        )
    }

    /// 按需记忆召回。失败或无命中都回填明确说明，让模型知道"查过了、没有"，
    /// 而不是拿到一个空串后误以为没查过。
    ///
    /// 回填**带 id**：模型要改写一条旧记忆时，唯一的 id 来源就是召回结果。
    fn recall(&self, query: &str) -> String {
        let started = std::time::Instant::now();
        match context::fetch_recall(self.memory, query, context::RECALL_LIMIT) {
            Ok(hits) => {
                tracing::info!(
                    query = %crate::utils::trace::Text(query),
                    hits = hits.len(),
                    ms = started.elapsed().as_millis() as u64,
                    "recall_memory 工具执行完成"
                );
                let body = context::id_lines(&hits);
                if body.trim().is_empty() {
                    "没有检索到相关记忆。".to_string()
                } else {
                    body
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "recall_memory 工具执行失败");
                format!("记忆检索失败：{e}")
            }
        }
    }

    /// 模型主动写入一条记忆。
    ///
    /// 失败在这里被吞成 `Err(String)`：记忆是辅助能力，一条记不住不该把整轮对话
    /// 拖失败。`dispatch` 会把错误文案回填给模型，让它下轮自己改正。
    fn remember(
        &self,
        content: &str,
        kind: MemoryKind,
        supersedes: Option<&str>,
    ) -> std::result::Result<crate::core::memory::WriteKind, String> {
        let started = std::time::Instant::now();
        let message_id = (!self.user_message_id.is_empty()).then_some(self.user_message_id);
        match self.memory.store(content, kind, supersedes, message_id) {
            Ok(outcome) => {
                tracing::info!(
                    memory_id = %outcome.memory.id,
                    kind = ?outcome.kind,
                    memory_type = ?outcome.memory.memory_type,
                    superseded = supersedes.unwrap_or(""),
                    content = %crate::utils::trace::Text(&outcome.memory.content),
                    ms = started.elapsed().as_millis() as u64,
                    "remember 工具写入完成"
                );
                tracing::info!(
                    target: "trace",
                    stage = "memory_written",
                    memory_id = %outcome.memory.id,
                    kind = ?outcome.kind,
                    content = %crate::utils::trace::Text(&outcome.memory.content),
                    "记忆写入完成"
                );
                self.written.borrow_mut().push(outcome.memory.clone());
                Ok(outcome.kind)
            }
            Err(e) => {
                tracing::warn!(error = %e, "remember 工具写入失败");
                Err(e.to_string())
            }
        }
    }
}

/// 投递一条消息：先归一化空白，再回调宿主，最后单独入库。
///
/// **每条消息单独落一行**是刻意的——多条 assistant 消息连续入库，正是"一句一句回"
/// 在时间线上的真实形态，也让后续按轮次回顾时不会把它们粘成一大段。
fn emit<F>(
    on_message: &mut F,
    delivered: &mut Vec<String>,
    conversation: &Sessions,
    session_id: &str,
    text: &str,
) -> Result<()>
where
    F: FnMut(&str) + Send,
{
    let text = crate::utils::textflow::flatten(text);
    if text.trim().is_empty() {
        return Ok(());
    }
    // 顺序刻意是「先入库、再回调」：回调要跨同步边界投递到通道（异步发送），
    // 期间可能被取消或超时；先落库能保证「发出去过」和「记下来过」一致，
    // 不会出现用户收到消息但时间线里查无此行的空洞。
    conversation.add_message(session_id, "assistant", &text)?;
    on_message(&text);
    delivered.push(text);
    Ok(())
}

/// 将 RFC3339 字符串解析为 UTC 时刻（解析失败视为未提供，回退接收时刻）。
fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// 延迟消息的时差说明块（英文给 LLM），拼在 system prompt 末尾。
///
/// 让模型知道"这条是 3 小时前发的"，由它自己决定怎么接——真人看到隔了几小时的
/// 消息会说"刚看到"，不会假装刚收到。这是示例之外必须显式告知的信息：模型看不到
/// 通道层的时间戳，也无从推断。
fn delay_notice(user_time: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    let sent_at = parse_rfc3339(user_time?)?;
    if !matches!(
        crate::core::session::classify_inbound(sent_at, now),
        crate::core::session::InboundFreshness::Delayed
    ) {
        return None;
    }
    let age = now.signed_duration_since(sent_at);
    let hours = age.num_hours().max(0);
    let when = if hours >= 1 {
        format!("about {hours} hour{}", if hours == 1 { "" } else { "s" })
    } else {
        format!("about {} minutes", age.num_minutes().max(1))
    };
    Some(format!(
        "\n\nNote: the other person sent this {when} ago, and you are only just seeing it now. \
         Don't pretend it just arrived, and don't apologize for the delay. Reply the way a \
         real person would when they finally check their phone: either pick the conversation \
         back up naturally, or briefly acknowledge that you're only just seeing it. \
         Never mention waiting, being offline, or how long it took."
    ))
}

/// 把消息数组渲染成 `[{role, content, tool_calls}, ...]` 的 JSON 数组文本
/// （追踪日志用，与真实请求的线协议结构一致）。
fn render_messages(messages: &[ChatMessage]) -> String {
    serde_json::to_string(
        &messages
            .iter()
            .map(|m| {
                let mut obj = serde_json::json!({ "role": m.role, "content": m.content });
                if !m.tool_calls.is_empty() {
                    obj["tool_calls"] = serde_json::json!(m
                        .tool_calls
                        .iter()
                        .map(|c| serde_json::json!({
                            "name": c.fn_name,
                            "arguments": c.fn_arguments,
                        }))
                        .collect::<Vec<_>>());
                }
                obj
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string())
}
