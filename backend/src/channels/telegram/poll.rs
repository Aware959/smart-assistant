//! Telegram poll 子模块。

use super::*;

/// 过期的语音消息入库时留下的占位文本（只入库不回复）。
pub(super) const EXPIRED_VOICE_PLACEHOLDER: &str = "[对方发来一条语音（已过期）]";

/// 语音/音频入站时的回话：听写能力已下线，直说比默默忽略好——用户知道自己发的
/// 东西没被听见，而不是以为对方没回。
pub(super) const VOICE_UNSUPPORTED_HINT: &str = "我这边暂时听不了语音，发文字给我吧～";

/// 长轮询入口。token 为空时直接返回，由 server 入口决定是否调用。
/// 异常退出后自动重连（启动期网络抖动不再导致永久掉线）。
pub async fn run(chat: Arc<Agent>, session: Arc<Sessions>, identity: Arc<IdentityService>) {
    let token = Config::get().telegram_bot_token.trim().to_string();
    if token.is_empty() {
        return;
    }
    loop {
        match run_loop(chat.clone(), session.clone(), identity.clone(), &token).await {
            Ok(()) => break,
            Err(e) => {
                tracing::error!(error = %e, "telegram 轮询异常退出，60s 后重连");
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    }
}

pub(super) async fn run_loop(
    chat: Arc<Agent>,
    session: Arc<Sessions>,
    identity: Arc<IdentityService>,
    token: &str,
) -> Result<(), String> {
    let client = build_client()?;
    let base = api_base(token);

    // 校验 token。
    let me: ApiResponse<serde_json::Value> = api_get(&client, &format!("{base}/getMe")).await?;
    if !me.ok {
        return Err(format!(
            "getMe 失败：{}",
            me.description.unwrap_or_default()
        ));
    }
    tracing::info!("telegram bot 已连接，开始长轮询");
    super::push_ready().store(true, std::sync::atomic::Ordering::Relaxed);

    // 拉取游标：从上次确认的位置续传，而不是跳到最新把离线期消息整段丢弃。
    //
    // Telegram 服务端会保留 update 约 24 小时，进程重启后仍能取回这期间的消息。
    // 取回后按消息真实时刻做时效分级（见 run_loop）：太旧的只入库不回复，
    // 有延迟的交给模型自己决定怎么接。用户因此不必为"上次没被收到"重发一遍。
    //
    // 首次运行（无游标）从 0 开始，即领取服务端保留的全部历史——此时几乎必然
    // 全是超时效的旧消息，会被 archive 分支挡掉，不产生任何回复。
    let mut offset: i64 = match session
        .load_cursor(CHANNEL)
        .as_deref()
        .and_then(|s| s.parse().ok())
    {
        Some(saved) => {
            tracing::info!(offset = saved, "telegram 续传游标，从上次位置继续");
            saved
        }
        None => {
            tracing::info!("telegram 无历史游标，从 0 开始领取服务端保留的 update");
            0
        }
    };

    let allowed = Config::get().telegram_allowed_ids.clone();

    loop {
        let url = format!(
            "{base}/getUpdates?timeout={POLL_TIMEOUT_SECS}&offset={offset}&allowed_updates=%5B%22message%22%5D"
        );
        let updates: ApiResponse<Vec<Update>> = match api_get(&client, &url).await {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(error = %e, "getUpdates 失败，{RETRY_DELAY_SECS}s 后重试");
                tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECS)).await;
                continue;
            }
        };
        if !updates.ok {
            tracing::warn!(
                description = ?updates.description,
                "getUpdates 返回 ok=false，重试"
            );
            tokio::time::sleep(Duration::from_secs(RETRY_DELAY_SECS)).await;
            continue;
        }

        let batch = updates.result.unwrap_or_default();
        if batch.is_empty() {
            // 长轮询正常空转，不刷日志（进程存活与否由 watchdog 探针单独确认）。
            continue;
        }
        tracing::info!(updates = batch.len(), "getUpdates 轮询返回");

        // 整批确认后立即落盘游标：即使后续处理中途崩溃，重启也不会重放这批。
        offset = offset.max(
            batch
                .iter()
                .map(|u| u.update_id + 1)
                .max()
                .unwrap_or(offset),
        );
        session.save_cursor(CHANNEL, &offset.to_string());

        for update in batch {
            offset = offset.max(update.update_id + 1);
            let Some(message) = update.message else {
                continue;
            };
            let chat_id = message.chat.id;
            if !allowed.is_empty() && !allowed.contains(&chat_id) {
                tracing::info!(chat_id, "未在白名单，忽略");
                continue;
            }
            let user_time = super::super::ts_to_rfc3339(message.date);
            let sent_at = user_time.as_deref().and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            });

            // 时效分级：离线积压的旧消息不该被当成刚收到的实时消息来回。
            //
            // 【前移】这一步排在**任何下载/转写之前**：过期语音不该为它永远不会被回复的
            // 结局付一遍 ASR 成本。原先它排在内容解析之后，语音因此没法先被识别出来。
            let freshness = sent_at
                .map(|at| classify_inbound(at, Utc::now()))
                .unwrap_or(InboundFreshness::Fresh);

            // 本条消息的内容来源（文本 / 贴纸 / 音频附件）；都不是则整条忽略（既有行为）。
            let Some(content) = message.resolve_inbound() else {
                continue;
            };

            let text = match content {
                // 过期的文本/贴纸：存档不回复（行为与改动前完全一致）。
                Inbound::Text(text) if freshness == InboundFreshness::TooOld => {
                    tracing::info!(chat_id, "消息已超时效，只入库不回复");
                    let _ = touch_user_reply(&session, &chat_id.to_string(), sent_at);
                    if let Err(e) = session.archive_only(
                        CHANNEL,
                        &chat_id.to_string(),
                        &text,
                        sent_at.unwrap_or_else(Utc::now),
                    ) {
                        tracing::warn!(error = %e, chat_id, "过期消息入库失败");
                    }
                    continue;
                }
                // 过期语音：只落一条占位说明，**不下载、不转写**。
                Inbound::Audio if freshness == InboundFreshness::TooOld => {
                    tracing::info!(chat_id, "语音已超时效，只入库不转写");
                    let _ = touch_user_reply(&session, &chat_id.to_string(), sent_at);
                    if let Err(e) = session.archive_only(
                        CHANNEL,
                        &chat_id.to_string(),
                        EXPIRED_VOICE_PLACEHOLDER,
                        sent_at.unwrap_or_else(Utc::now),
                    ) {
                        tracing::warn!(error = %e, chat_id, "过期语音入库失败");
                    }
                    continue;
                }
                Inbound::Text(text) => text,
                // 语音/音频入站：**不再做转写**（听写能力已下线）。
                // 回一句实话比默默忽略好——用户知道自己发的东西没被听见，
                // 而不是以为她没回。
                Inbound::Audio => {
                    send_typing(&client, &base, chat_id).await;
                    if let Err(e) = send_text(&client, &base, chat_id, VOICE_UNSUPPORTED_HINT).await
                    {
                        tracing::error!(error = %e, chat_id, "语音提示发送失败");
                    }
                    continue;
                }
            };

            tracing::info!(
                chat_id,
                ?freshness,
                "收到消息，开始处理（进入 AI 流水线前）"
            );

            // 会话映射先建起来：下面的入库、起轮次都要 session id，建不起来这轮就没法进行。
            let Some(session_id) = touch_user_reply(&session, &chat_id.to_string(), sent_at) else {
                continue;
            };

            // 命令类消息（`/start` / `/new` / 身份管理）在异步侧直接回执，不进 AI 流水线，
            // 也就不必起投递工人与阻塞线程。
            let is_admin = allowed.is_empty() || allowed.contains(&chat_id);
            match command_reply(&session, &identity, is_admin, chat_id, &text) {
                Ok(Some(reply)) => {
                    send_typing(&client, &base, chat_id).await;
                    if let Err(e) = send_text(&client, &base, chat_id, &reply).await {
                        tracing::error!(error = %e, chat_id, "命令回执发送失败");
                    }
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::error!(error = %e, chat_id, "命令处理失败");
                    continue;
                }
            }

            // 用户消息**落库先于起轮次**——这是"运行中插话"的前提：一轮在跑时新到的消息
            // 不再排队等它跑完，而是先躺在时间线上，由那一轮在迭代边界取用（`core::agent`
            // 的插话机制）。落库用报文里的真实发出时刻，时间线不受处理延迟影响。
            let persisted = match sent_at {
                Some(at) => session.add_message_at(&session_id, "user", &text, at),
                None => session.add_message(&session_id, "user", &text),
            };
            let user_message_id = match persisted {
                Ok(m) => m.id,
                Err(e) => {
                    tracing::error!(error = %e, chat_id, "用户消息入库失败，本轮作罢（不入库不回）");
                    continue;
                }
            };

            send_typing(&client, &base, chat_id).await;

            // 同一会话同时只跑一轮：已有轮次在跑时**不再另起一轮**，刚落的这条消息由
            // 那一轮在迭代边界取用；取不上（这轮已收尾）也仍在时间线里，下次对话照读得到。
            let Some(ticket) = session.begin_turn(&session_id) else {
                tracing::info!(
                    chat_id,
                    "本会话已有轮次在跑，消息入库后交给它，不再另起一轮"
                );
                continue;
            };

            spawn_turn(
                chat.clone(),
                session.clone(),
                client.clone(),
                base.clone(),
                chat_id,
                session_id,
                text,
                user_time,
                user_message_id,
                ticket,
            );
        }
    }
}
