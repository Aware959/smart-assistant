//! Runtime host 的本地控制接口。
//!
//! 当前由 `zhiwei-service` 托管，桌面 / Linux / Android 客户端复用同一契约。
//! 接口只绑定 loopback，并要求启动时生成的 Bearer token；客户端不得直接写数据库。

mod auth;
pub mod client;
mod http;

pub use http::serve;
pub use http::ServeParams;
