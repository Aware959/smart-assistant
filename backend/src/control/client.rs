//! 本地控制接口客户端：供运维 CLI 复用。
//!
//! 只读取 `zhiwei-service` 写出的状态文件和 token，不直接打开数据库。

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::config::Config;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct ControlState {
    address: String,
}

pub struct ControlClient {
    address: String,
    token: String,
}

impl ControlClient {
    pub fn from_config(cfg: &Config) -> Result<Self, String> {
        let state: ControlState = serde_json::from_str(
            &fs::read_to_string(&cfg.control_state_file).map_err(|error| {
                format!(
                    "控制接口尚未就绪（{}）：{error}。请先启动 zhiwei-service。",
                    cfg.control_state_file
                )
            })?,
        )
        .map_err(|error| format!("控制状态文件无效：{error}"))?;
        let token = fs::read_to_string(&cfg.control_token_file).map_err(|error| {
            format!("无法读取控制 token（{}）：{error}", cfg.control_token_file)
        })?;
        Ok(Self {
            address: state.address,
            token: token.trim().to_string(),
        })
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        self.request("GET", path, None)
    }

    pub fn patch<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, String> {
        self.request(
            "PATCH",
            path,
            Some(
                serde_json::to_value(body)
                    .map_err(|error| format!("控制请求序列化失败：{error}"))?,
            ),
        )
    }

    pub fn post<T: DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, String> {
        self.request(
            "POST",
            path,
            Some(
                serde_json::to_value(body)
                    .map_err(|error| format!("控制请求序列化失败：{error}"))?,
            ),
        )
    }

    fn request<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, String> {
        let address = self
            .address
            .strip_prefix("http://")
            .ok_or_else(|| "控制接口只支持 loopback HTTP".to_string())?;
        let mut stream = TcpStream::connect(address)
            .map_err(|error| format!("无法连接控制接口 {address}：{error}"))?;
        let _ = stream.set_read_timeout(Some(CONTROL_TIMEOUT));
        let _ = stream.set_write_timeout(Some(CONTROL_TIMEOUT));
        let body = body
            .map(|value| serde_json::to_string(&value))
            .transpose()
            .map_err(|error| format!("控制请求序列化失败：{error}"))?
            .unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.token,
            body.len()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| format!("控制请求发送失败：{error}"))?;
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .map_err(|error| format!("控制响应读取失败：{error}"))?;
        let (status, body) = parse_http_response(&response)?;
        if !(200..300).contains(&status) {
            return Err(format!("控制接口返回 {status}：{body}"));
        }
        serde_json::from_str(&body).map_err(|error| format!("控制接口响应无效：{error}"))
    }
}

fn parse_http_response(response: &str) -> Result<(u16, String), String> {
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "控制响应缺少 HTTP 头".to_string())?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| "控制响应状态码无效".to_string())?;
    Ok((status, body.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_status_and_body() {
        assert_eq!(
            parse_http_response("HTTP/1.1 200 OK\r\n\r\n{\"ok\":true}").unwrap(),
            (200, "{\"ok\":true}".to_string())
        );
        assert!(parse_http_response("not-http").is_err());
    }
}
