//! 主动推送的跨通道路由。
//!
//! 主动引擎经 [`ChannelsPusher`] 查询各通道能否外发并派发文本。各通道的
//! 运行状态与协议细节由各自子模块维护，这里只做名称到具体实现的转译。

/// 主动外发的具体实现：转译到具体通道派发。
///
/// 无状态，查询各通道自己的运行状态与配置；由 bin 接线时注入主动引擎，
/// 延续会话则直接由各 relay 构造零成本实例传入。
#[derive(Default)]
pub struct ChannelsPusher;

impl ChannelsPusher {
    /// 该通道当前是否具备外发条件（电报已连通 / iLink token 仍在新鲜窗口）。
    #[cfg_attr(not(feature = "ilink"), allow(unused_variables))]
    pub fn pushable(&self, channel: &str, external_id: &str) -> bool {
        match channel {
            #[cfg(feature = "telegram")]
            "telegram" => super::telegram::pushable(),
            #[cfg(not(feature = "telegram"))]
            "telegram" => false,
            #[cfg(feature = "ilink")]
            "ilink" => ilink_has_fresh_token(external_id),
            #[cfg(not(feature = "ilink"))]
            "ilink" => false,
            other => {
                tracing::warn!(channel = other, "未知通道，无法主动推送");
                false
            }
        }
    }

    /// 外发一条文本消息（异步派发，内部处理各通道协议）。
    pub async fn push(
        &self,
        channel: String,
        external_id: String,
        text: String,
    ) -> std::result::Result<(), String> {
        #[cfg(feature = "telegram")]
        if channel == "telegram" {
            return super::telegram::push_text(&external_id, &text).await;
        }
        #[cfg(not(feature = "telegram"))]
        if channel == "telegram" {
            tracing::warn!("telegram 通道未启用");
            return Err("telegram 通道未启用".to_string());
        }
        #[cfg(feature = "ilink")]
        if channel == "ilink" {
            return super::ilink::push_send(&external_id, &text).await;
        }
        #[cfg(not(feature = "ilink"))]
        if channel == "ilink" {
            tracing::warn!("ilink 通道未启用");
            return Err("ilink 通道未启用".to_string());
        }
        tracing::warn!(channel = channel, "暂不支持主动推送的通道");
        Err(format!("未知通道 {channel}"))
    }
}

#[cfg(feature = "ilink")]
fn ilink_has_fresh_token(external_id: &str) -> bool {
    super::ilink::pushable(external_id)
}
