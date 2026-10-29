//! 出站发送重试原语。

/// 单次出站尝试的失败原因，区分“退避后重投可能成功”与“重投必然再失败”。
pub(crate) enum SendErr {
    /// 传输层失败、429 限流、5xx、响应被截断：值得重投。
    /// `retry_after` 是服务端指定的等待时长（如 Telegram 429 的 `parameters.retry_after`）。
    Retryable {
        reason: String,
        retry_after: Option<std::time::Duration>,
    },
    /// 4xx、Bot API 的 `ok:false`、本地构造错误：重投只会原样再失败一次。
    Fatal(String),
}

impl std::fmt::Display for SendErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendErr::Retryable { reason, .. } | SendErr::Fatal(reason) => f.write_str(reason),
        }
    }
}

impl From<String> for SendErr {
    /// 请求头/报文在本地构造失败，属配置问题，重投无意义。
    fn from(reason: String) -> Self {
        SendErr::Fatal(reason)
    }
}

/// 该 HTTP 状态码是否值得重投：429 限流与 5xx 服务端故障。
/// 4xx 是确定性拒绝（参数错、无权限、拉黑），重投只是白等。
pub(crate) fn status_retryable(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

/// 出站重试策略：最大尝试次数 + 首次退避，随后指数增长至封顶。
#[derive(Clone, Copy)]
pub(crate) struct SendRetry {
    pub max_attempts: u32,
    pub base_delay: std::time::Duration,
    pub max_delay: std::time::Duration,
}

/// 生产策略：VPN 闪断 / 弱网抖动通常在数秒到十几秒内自行恢复，
/// 1+2+4+6 秒的退避给出约 13 秒的累计窗口，足以覆盖一次隧道重建。
/// 再久就会把长轮询卡住，得不偿失——那属于该做持久化重投的范畴。
pub(crate) const SEND_RETRY: SendRetry = SendRetry {
    max_attempts: 5,
    base_delay: std::time::Duration::from_secs(1),
    max_delay: std::time::Duration::from_secs(6),
};

impl SendRetry {
    /// 第 `attempt` 次失败（从 0 起）后应等待多久。服务端给了 `retry_after` 就听服务端的，
    /// 但仍受 `max_delay` 封顶，避免被一个异常大的 `retry_after` 挂住整个轮询。
    pub(crate) fn resolve_delay(
        &self,
        attempt: u32,
        hint: Option<std::time::Duration>,
    ) -> std::time::Duration {
        match hint {
            Some(h) => h.min(self.max_delay),
            None => self
                .base_delay
                .saturating_mul(1u32.checked_shl(attempt).unwrap_or(u32::MAX))
                .min(self.max_delay),
        }
    }
}

/// 把一次出站请求包成“失败退避重投”的调用。
///
/// `run` 每次收到当前尝试序号（从 0 起），按 [`SendErr`] 分类决定重投还是放弃；
/// 彻底失败才把最后一次的原因交回调用方，由调用方判定“这条没发出去”。
///
/// 不做跨进程/跨重启的持久化重投：那是 outbox 的职责，不在这里。
pub(crate) async fn with_send_retry<T, F, Fut>(
    what: &str,
    policy: SendRetry,
    mut run: F,
) -> Result<T, String>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<T, SendErr>>,
{
    let mut attempt = 0u32;
    loop {
        match run(attempt).await {
            Ok(v) => return Ok(v),
            Err(SendErr::Fatal(reason)) => return Err(reason),
            Err(SendErr::Retryable {
                reason,
                retry_after,
            }) => {
                if attempt + 1 >= policy.max_attempts {
                    return Err(reason);
                }
                let delay = policy.resolve_delay(attempt, retry_after);
                tracing::warn!(
                    what,
                    attempt = attempt + 1,
                    max_attempts = policy.max_attempts,
                    delay_ms = delay.as_millis() as u64,
                    error = %reason,
                    "发送失败，退避后重试"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}
