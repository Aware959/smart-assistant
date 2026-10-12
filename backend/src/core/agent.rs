//! 编排内核（Agent）：把会话 + 能力组件组装成可复用的业务流水线。
//!
//! 重构后会话是 core 内具体类型（[`Sessions`]），能力直接以具体类型接线：
//! LLM = [`crate::llm::chat::Llm`]，记忆 = [`crate::core::memory::Store`]。

use std::sync::Arc;

use crate::core::context::{self, AgentContext};
use crate::core::memory::{MemoryKind, MemoryRecord, Store};
use crate::core::schema::message::ChatMessage;
use crate::core::self_profile::Profile;
use crate::core::session::Sessions;
use crate::core::sticker::StickerCatalog;
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

/// 一条投递给用户的回复片段。
///
/// 文本只是其中一种形态。把"回复"建模成有序片段列表，通道才能表达骰子、贴纸
/// 这类**非文本输出**；[`ChatOutput::messages`] 仍是它的纯文本投影，供只需文本的
/// 宿主（CLI、Web、iLink）使用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// 一条文本消息。
    Text(String),
    /// 一枚 Telegram 动画骰子（服务端渲染，无需素材）。`emoji` 为基座 emoji。
    Dice { emoji: String },
    /// 一张贴纸（表情包）。`file_id` 来自 [`crate::core::sticker::StickerCatalog`]，
    /// 选择键不进片段——键可能被手改，历史里留键反而会在重拉目录后指向别的图。
    Sticker { file_id: String },
}

impl Reply {
    /// 纯文本投影：文本原样，非文本给一个人类与模型都能读懂的占位描述。
    ///
    /// 用于历史入库与 [`ChatOutput::messages`] / [`ChatOutput::reply`]——
    /// 让不识别非文本的宿主与后续轮次的模型至少知道"这里发生过什么"。
    pub fn projection(&self) -> String {
        match self {
            Reply::Text(text) => text.clone(),
            Reply::Dice { emoji } => format!("[发了骰子 {emoji}]"),
            Reply::Sticker { .. } => "[发了一个表情包]".to_string(),
        }
    }

    /// 若为文本片段则返回其内容。
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Reply::Text(text) => Some(text),
            Reply::Dice { .. } | Reply::Sticker { .. } => None,
        }
    }
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
    /// 这是 [`Self::rich`] 的**纯文本投影**，不识别非文本的宿主用它即可。
    pub messages: Vec<String>,
    /// 本轮投递的**有序原始片段**（含文本与非文本），顺序即投递顺序。
    ///
    /// 能表达非文本输出的通道（Telegram）按这个列表投递；`messages` 是它的
    /// 文本投影。两者长度可能不同（一个骰子不是一条文本消息）。
    pub rich: Vec<Reply>,
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
    /// 贴纸目录。装配时加载一次：拉新目录要重启进程才生效（目录是文件不是状态）。
    stickers: StickerCatalog,
}

impl Agent {
    /// 用具体会话适配器与能力组件构造内核。
    pub fn new(
        conversation: Arc<Sessions>,
        llm: Arc<Llm>,
        memory: Arc<Store>,
        profile: Arc<Profile>,
        stickers: StickerCatalog,
    ) -> Self {
        Self {
            conversation,
            llm,
            memory,
            profile,
            stickers,
        }
    }

    /// 自我属性人设区块文本（主动决策复用同一人设数据源）。
    pub(crate) fn profile_block(&self) -> String {
        self.profile.block()
    }

    /// 最近 `limit` 条对话的渲染文本（主动决策只需历史的场景，避免白跑一次召回）。
    pub(crate) fn recent_history_text(&self, session_id: &str, limit: usize) -> String {
        context::recent_history_text(self.conversation.as_ref(), session_id, limit)
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
    pub fn chat_stream<F>(&self, input: &ChatInput, on_message: F) -> Result<ChatOutput>
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
                rich: Vec::new(),
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

        // 4. 工具循环。对话路径必须产出一条消息：没产出前由循环在协议层强制
        // 模型调工具（见 [`ToolPolicy`]），空投递仍交给下面的判据报错兜底。
        let tools = crate::llm::tool::definitions(&self.stickers);
        let (delivered, last_memory) = self.run_tool_loop(
            &session.id,
            &user_message.id,
            messages,
            &tools,
            ToolPolicy::MustReply,
            on_message,
        )?;

        // 对话路径一条消息都没产出 = 失败。主动路径的"空"是"决定不开口"，
        // 所以那条判据留在各自的入口，不放进共用的循环里。
        if delivered.is_empty() {
            return Err(crate::error::SqlError::Config(
                "模型未产出任何可发送的消息".to_string(),
            ));
        }
        // `messages`/`reply` 是 `rich` 的文本投影：非文本片段（骰子）在这里被
        // 渲染成占位描述，让不识别它的宿主与后续轮次模型仍有迹可循。
        let messages: Vec<String> = delivered.iter().map(Reply::projection).collect();
        let reply = messages.join("\n");

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

        Ok(ChatOutput {
            session_id: session.id,
            reply,
            messages,
            rich: delivered,
            user_message_id: user_message.id,
            memory: last_memory,
        })
    }

    /// 主动开口主入口（定时触发，没有用户消息）。
    ///
    /// 与 [`Self::chat_stream`] 共用同一套工具循环，区别只在：提示词换成主动版本、
    /// 消息里注入当前时间与最后一次互动时刻、工具集去掉 `remember`、**空投递是正常
    /// 结果**（模型决定不开口）。产出的每条消息仍由 [`emit`] 落库并回调——因此调用方
    /// 只需把返回的消息按通道推出去，历史已在内核里写好。
    ///
    /// 返回 `Ok(vec![])` 表示"决定不开口"；`Err` 表示这一轮决策因异常中断（调用方
    /// 同样按不开口处理即可）。
    pub fn proactive_stream<F>(
        &self,
        session_id: &str,
        now: chrono::DateTime<chrono::Utc>,
        last_user_reply_at: Option<chrono::DateTime<chrono::Utc>>,
        last_assistant_at: Option<chrono::DateTime<chrono::Utc>>,
        on_message: F,
    ) -> Result<Vec<String>>
    where
        F: FnMut(&str) + Send,
    {
        let recent = self.recent_history_text(session_id, PROACTIVE_HISTORY_TURNS);
        let profile = self.profile_block();
        let system = context::proactive_system_prompt(&profile);
        let user =
            context::proactive_user_prompt(&recent, now, last_user_reply_at, last_assistant_at);
        let messages = vec![ChatMessage::system(system), ChatMessage::user(user)];
        tracing::info!(
            target: "trace",
            stage = "proactive_prompt",
            session_id,
            n = messages.len(),
            "主动决策：提示词已组装"
        );

        let tools = crate::llm::tool::proactive_definitions();
        let (delivered, _) = self.run_tool_loop(
            session_id,
            "",
            messages,
            &tools,
            ToolPolicy::MayStaySilent,
            on_message,
        )?;
        // 主动路径的工具集不含 `send_dice` / `send_sticker`，故这里只可能是文本；仍走
        // 投影以保持统一口径（若将来给主动路径放开非文本，这里改回 `Vec<Reply>` 即可）。
        Ok(delivered.into_iter().map(|r| r.projection()).collect())
    }

    /// 工具循环：流式生成 → 有工具调用则执行并回填、无工具调用则结束。
    ///
    /// 对话与主动两条路径共用这一份。`tool_policy` 决定"还没产出用户可见消息时"要不要
    /// 在**协议层**强制模型调工具——见 [`ToolPolicy`]，这是治"模型绕过工具直接写正文"的
    /// 手段，不是提示词软约束。
    ///
    /// 返回 `(本轮投递的消息, 最后写入的记忆)`；"空投递算不算失败"仍由调用方决定。
    fn run_tool_loop<F>(
        &self,
        session_id: &str,
        user_message_id: &str,
        mut messages: Vec<ChatMessage>,
        tools: &[genai::chat::Tool],
        tool_policy: ToolPolicy,
        mut on_message: F,
    ) -> Result<(Vec<Reply>, Option<MemoryRecord>)>
    where
        F: FnMut(&str) + Send,
    {
        // 写入过的记忆留给调用方回报宿主；工具上下文是共享引用，
        // 所以这里用内部可变性收集，不能靠 &mut。
        let tool_ctx = TurnTools {
            memory: self.memory.as_ref(),
            user_message_id,
            stickers: &self.stickers,
            written: std::cell::RefCell::new(Vec::new()),
        };
        let llm_phase = std::time::Instant::now();
        let mut delivered: Vec<Reply> = Vec::new();
        // 连续多少轮只调只读工具、一次都没说话。防止模型在工具上来回打转不说话。
        let mut mute_rounds = 0u32;

        for iter in 1..=MAX_TOOL_ITER {
            // 协议层工具选择：对话路径在"一条消息都还没发出"前强制模型调工具。
            // 实测本仓的 DeepSeek-V4.1-Flash 在同一上下文里 auto 下 0/3 会直接写正文、
            // `Required` 下 3/3 走 send_message，软提示拦不住。一旦说上话（delivered 非空）
            // 就放开，让模型能自然收尾；没有工具可选时不能传（provider 会拒绝）。
            let tool_choice = match tool_policy {
                ToolPolicy::MustReply if delivered.is_empty() && !tools.is_empty() => {
                    Some(genai::chat::ToolChoice::Required)
                }
                _ => None,
            };
            // 直接正文**整体丢弃**：给用户看的话只能由 `send_message` 工具产出。模型绕过
            // 工具写的正文（旁白 / 铺垫）既不在这里下发，也不在收尾入库——所以这个
            // `on_delta` 是空实现，流式增量同样不外发。
            let mut on_delta = |_piece: &str| {};
            let outcome = match self.llm.complete_stream_with_tools(
                &messages,
                tools,
                tool_choice,
                &mut on_delta,
            ) {
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
            //
            // 正文一律丢弃、不下发不入库：模型绕过 `send_message` 直接写进 content 的原话
            // （多为内心旁白 / 开场铺垫）不是给用户看的话。没有工具调用又没说话，就是
            // "这一轮不开口"，正常结束（对话路径的空投递由调用方判为失败）。
            if !outcome.has_tool_calls() {
                if !outcome.text.trim().is_empty() {
                    tracing::warn!(
                        session = %session_id,
                        iter,
                        chars = outcome.text.chars().count(),
                        forced = matches!(tool_policy, ToolPolicy::MustReply) && delivered.is_empty(),
                        "模型绕过工具直接写了正文，按约定丢弃（不投递、不入库）"
                    );
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
            // 工具往返落库（顺序保证 genai 协议合法，见 `emit` 注释）：先 assistant
            // 调用行（含 tool_calls JSON），再逐个工具结果行，投递文本延后到迭代末尾。
            let tool_calls_json = serde_json::to_string(&outcome.tool_calls).ok();
            messages.push(ChatMessage::assistant_calls(
                outcome.text.clone(),
                outcome.tool_calls.clone(),
            ));
            self.conversation.add_message_full(
                session_id,
                "assistant",
                &outcome.text,
                &crate::db::message::ToolMeta {
                    tool_calls: tool_calls_json,
                    tool_call_id: None,
                    tool_name: None,
                },
            )?;

            let mut spoke = false;
            let mut delivered_records: Vec<String> = Vec::new();
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
                        // 工具结果先落库（role=assistant + tool_call_id 标记），
                        // 投递文本由 emit 回调后收集、迭代末尾统一落库。
                        self.conversation.add_message_full(
                            session_id,
                            "assistant",
                            crate::llm::tool::SEND_MESSAGE_ACK,
                            &crate::db::message::ToolMeta {
                                tool_calls: None,
                                tool_call_id: Some(call.call_id.clone()),
                                tool_name: Some(call.fn_name.clone()),
                            },
                        )?;
                        let record = emit(&mut on_message, &mut delivered, Reply::Text(text))?;
                        if !record.is_empty() {
                            delivered_records.push(record);
                        }
                        spoke = true;
                        messages.push(ChatMessage::tool_result(
                            call,
                            crate::llm::tool::SEND_MESSAGE_ACK,
                        ));
                    }
                    crate::llm::tool::ToolOutcome::SentDice(emoji) => {
                        tracing::info!(
                            session = %session_id,
                            emoji = %emoji,
                            "send_dice 已投递"
                        );
                        self.conversation.add_message_full(
                            session_id,
                            "assistant",
                            crate::llm::tool::SEND_DICE_ACK,
                            &crate::db::message::ToolMeta {
                                tool_calls: None,
                                tool_call_id: Some(call.call_id.clone()),
                                tool_name: Some(call.fn_name.clone()),
                            },
                        )?;
                        let record = emit(&mut on_message, &mut delivered, Reply::Dice { emoji })?;
                        if !record.is_empty() {
                            delivered_records.push(record);
                        }
                        spoke = true;
                        messages.push(ChatMessage::tool_result(
                            call,
                            crate::llm::tool::SEND_DICE_ACK,
                        ));
                    }
                    crate::llm::tool::ToolOutcome::SentSticker(file_id) => {
                        tracing::info!(session = %session_id, "send_sticker 已投递");
                        self.conversation.add_message_full(
                            session_id,
                            "assistant",
                            crate::llm::tool::SEND_STICKER_ACK,
                            &crate::db::message::ToolMeta {
                                tool_calls: None,
                                tool_call_id: Some(call.call_id.clone()),
                                tool_name: Some(call.fn_name.clone()),
                            },
                        )?;
                        let record =
                            emit(&mut on_message, &mut delivered, Reply::Sticker { file_id })?;
                        if !record.is_empty() {
                            delivered_records.push(record);
                        }
                        spoke = true;
                        messages.push(ChatMessage::tool_result(
                            call,
                            crate::llm::tool::SEND_STICKER_ACK,
                        ));
                    }
                    crate::llm::tool::ToolOutcome::Filled(text) => {
                        self.conversation.add_message_full(
                            session_id,
                            "assistant",
                            &text,
                            &crate::db::message::ToolMeta {
                                tool_calls: None,
                                tool_call_id: Some(call.call_id.clone()),
                                tool_name: Some(call.fn_name.clone()),
                            },
                        )?;
                        messages.push(ChatMessage::tool_result(call, text));
                    }
                    crate::llm::tool::ToolOutcome::Rejected(reason) => {
                        tracing::warn!(
                            session = %session_id,
                            tool = %call.fn_name,
                            reason = %reason,
                            "工具调用被拒绝"
                        );
                        let content = format!("调用失败：{reason}");
                        self.conversation.add_message_full(
                            session_id,
                            "assistant",
                            &content,
                            &crate::db::message::ToolMeta {
                                tool_calls: None,
                                tool_call_id: Some(call.call_id.clone()),
                                tool_name: Some(call.fn_name.clone()),
                            },
                        )?;
                        messages.push(ChatMessage::tool_result(call, content));
                    }
                }
            }

            // 投递文本延后到工具结果之后落库，保证协议顺序
            // `assistant(调用) → tool(结果)… → assistant(投递文本)`。
            for record in &delivered_records {
                self.conversation
                    .add_message(session_id, "assistant", record)?;
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
        tracing::info!(session = %session_id, llm_ms, "工具循环完成");

        // 本轮写入的最后一条记忆（若有）。取在 return 之前，
        // 让借用先结束——否则返回值会带着 Ref 活到函数尾。
        let last_memory = tool_ctx.written.borrow().last().cloned();
        Ok((delivered, last_memory))
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

/// 工具循环的"产出要求"策略：对话与主动两条路径的唯一区别。
///
/// 它管的不是提示词，而是**协议层**的工具选择（`tool_choice`）：模型绕过工具、把回话
/// 写进 `content` 是这套输出的主要失败模式，而软提示拦不住（实测同一上下文 auto 下
/// 0/3 命中、`Required` 下 3/3 命中）。所以"必须产出"这件事要落到 API 契约上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPolicy {
    /// 对话：用户发了话，必须先产出一条消息。在产出前用 `tool_choice = Required`
    /// 堵死"直接写正文"，产出后放开让模型自然收尾。空投递仍由调用方判为失败。
    MustReply,
    /// 主动：允许沉默。"一个工具都不调"就是"决定不开口"，是正常结果，不干预。
    MayStaySilent,
}

/// 主动决策读入的最近对话轮数。
///
/// 主动路径要判断"此刻该不该开口"，看的是最近聊到哪了，不需要跨天铺陈；
/// 且它由定时器驱动、频率远高于对话，读太多历史纯属浪费上下文。
const PROACTIVE_HISTORY_TURNS: usize = 8;

/// 一轮对话内的工具执行环境。
///
/// 只读工具与 `remember` 共用这一个对象：`remember` 要写状态，所以这里带上了本轮
/// 用户消息 id（作为记忆的来源）；只读工具用不到它，但让两条路径共用一份上下文
/// 比拆成两个对象少一层分发。
struct TurnTools<'a> {
    memory: &'a Store,
    /// 本条用户消息 id：记忆的 `message_id` 指向它。
    user_message_id: &'a str,
    /// 贴纸目录：`send_sticker` 的键 → `file_id` 校验全靠它。
    stickers: &'a StickerCatalog,
    /// 本轮写入的记忆，最后一条随 `ChatOutput` 回报宿主。
    written: std::cell::RefCell<Vec<MemoryRecord>>,
}

impl crate::llm::tool::ToolContext for TurnTools<'_> {
    /// 当地时间。复用 [`context::local_time_text`]——"今天"的起点和"现在几点"
    /// 必须用同一个时区与同一套格式。
    fn now(&self) -> String {
        context::local_time_text(chrono::Utc::now())
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

    /// 可用贴纸键。目录为空即"没拉过"，模型侧连工具都不会拿到。
    fn sticker_keys(&self) -> Vec<String> {
        self.stickers.keys()
    }

    fn sticker_file_id(&self, key: &str) -> Option<String> {
        self.stickers.file_id(key).map(str::to_string)
    }
}

/// 投递一个回复片段：归一化空白、回调宿主、记入本轮投递列表，返回入库用的文本。
///
/// **每个片段单独落一行**是刻意的——多条 assistant 消息连续入库，正是"一句一句回"
/// 在时间线上的真实形态，也让后续按轮次回顾时不会把它们粘成一大段。非文本片段
/// （骰子）以 [`Reply::projection`] 的占位描述入库，保证历史里留有痕迹。
///
/// 注意：本函数**只负责投递与回调**，不再落库——落库由调用方在迭代末尾统一做。
/// 原因见 [`run_tool_loop`]：genai 要求 assistant 的工具调用必须紧跟其 tool 结果，
/// 而一次响应可能并行吐出多条 `send_message`；若每条投递在分发时就地落库，工具结果
/// 行与投递文本会交错，重建出的历史会破坏协议顺序。故工具结果先落、投递文本延后到
/// 迭代末尾落，保证库里顺序恒为 `assistant(调用) → tool(结果)… → assistant(投递文本)`。
fn emit<F>(on_message: &mut F, delivered: &mut Vec<Reply>, reply: Reply) -> Result<String>
where
    F: FnMut(&str) + Send,
{
    // 文本要归一化空白；非文本按其投影入库（投影已是稳定文本，无需再折叠）。
    let (reply, record) = match reply {
        Reply::Text(text) => {
            let text = crate::utils::textflow::flatten(&text);
            (Reply::Text(text.clone()), text)
        }
        other => {
            let record = other.projection();
            (other, record)
        }
    };
    if record.trim().is_empty() {
        return Ok(String::new());
    }
    on_message(&record);
    delivered.push(reply);
    Ok(record)
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
