//! 出站重试原语的单元测试（见 [`super`] 的 `SendErr` / `SendRetry` / `with_send_retry`）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

/// 零等待的策略：让重试次数的断言不掺入真实退避时长。
const FAST: SendRetry = SendRetry {
    max_attempts: 4,
    base_delay: Duration::from_millis(0),
    max_delay: Duration::from_millis(0),
};

fn retryable(reason: &str) -> SendErr {
    SendErr::Retryable {
        reason: reason.to_string(),
        retry_after: None,
    }
}

#[test]
fn only_429_and_5xx_are_retryable() {
    for code in [429, 500, 502, 503, 504, 599] {
        assert!(status_retryable(code), "{code} 应可重投");
    }
    for code in [200, 301, 400, 401, 403, 404, 422] {
        assert!(!status_retryable(code), "{code} 不应重投");
    }
}

#[test]
fn backoff_grows_then_caps() {
    let policy = SendRetry {
        max_attempts: 8,
        base_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(6),
    };
    let seq: Vec<u64> = (0..8)
        .map(|a| policy.resolve_delay(a, None).as_millis() as u64)
        .collect();
    // 1s, 2s, 4s 后封顶 6s
    assert_eq!(seq, vec![1000, 2000, 4000, 6000, 6000, 6000, 6000, 6000]);
    // attempt 大到溢出移位时也不应 panic，仍落在封顶内
    assert_eq!(policy.resolve_delay(40, None), Duration::from_secs(6));
}

#[test]
fn server_retry_after_wins_over_backoff_but_stays_capped() {
    let policy = SendRetry {
        max_attempts: 5,
        base_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(6),
    };
    // attempt 0 原本等 1s，服务端要求 4s → 听服务端的
    assert_eq!(
        policy.resolve_delay(0, Some(Duration::from_secs(4))),
        Duration::from_secs(4)
    );
    // 服务端报了离谱的大值也不能把轮询挂住
    assert_eq!(
        policy.resolve_delay(0, Some(Duration::from_secs(600))),
        Duration::from_secs(6)
    );
}

#[tokio::test]
async fn succeeds_after_transient_failures() {
    let calls = Arc::new(Mutex::new(0u32));
    let seen = calls.clone();
    let out: Result<u32, String> = with_send_retry("test", FAST, move |attempt| {
        let seen = seen.clone();
        async move {
            *seen.lock().unwrap() += 1;
            if attempt < 2 {
                Err(retryable("连接被重置"))
            } else {
                Ok(attempt)
            }
        }
    })
    .await;
    assert_eq!(out, Ok(2));
    assert_eq!(
        *calls.lock().unwrap(),
        3,
        "应尝试 3 次：2 次失败 + 1 次成功"
    );
}

#[tokio::test]
async fn gives_up_after_max_attempts() {
    let calls = Arc::new(Mutex::new(0u32));
    let seen = calls.clone();
    let out: Result<(), String> = with_send_retry("test", FAST, move |_| {
        let seen = seen.clone();
        async move {
            *seen.lock().unwrap() += 1;
            Err(retryable("隧道不可达"))
        }
    })
    .await;
    assert_eq!(out, Err("隧道不可达".to_string()), "应交回最后一次原因");
    assert_eq!(
        *calls.lock().unwrap(),
        FAST.max_attempts,
        "总尝试次数应等于 max_attempts"
    );
}

#[tokio::test]
async fn fatal_error_is_not_retried() {
    let calls = Arc::new(Mutex::new(0u32));
    let seen = calls.clone();
    let out: Result<(), String> = with_send_retry("test", FAST, move |_| {
        let seen = seen.clone();
        async move {
            *seen.lock().unwrap() += 1;
            Err(SendErr::Fatal("bot 被拉黑".to_string()))
        }
    })
    .await;
    assert_eq!(out, Err("bot 被拉黑".to_string()));
    assert_eq!(*calls.lock().unwrap(), 1, "4xx 类失败只试一次");
}

#[tokio::test]
async fn single_attempt_policy_reports_first_failure() {
    let once = SendRetry {
        max_attempts: 1,
        base_delay: Duration::from_millis(0),
        max_delay: Duration::from_millis(0),
    };
    let calls = Arc::new(Mutex::new(0u32));
    let seen = calls.clone();
    let out: Result<(), String> = with_send_retry("test", once, move |_| {
        let seen = seen.clone();
        async move {
            *seen.lock().unwrap() += 1;
            Err(retryable("首次就失败"))
        }
    })
    .await;
    assert_eq!(out, Err("首次就失败".to_string()));
    assert_eq!(*calls.lock().unwrap(), 1);
}

#[tokio::test]
async fn production_policy_stays_within_bounded_window() {
    // 真实策略的累计退避必须有限，否则一次弱网就能把长轮询卡死。
    // 1+2+4+6 秒 —— 少于 4 次尝试时这段求和才是完整的 4 次退避。
    let total: u64 = (0..SEND_RETRY.max_attempts - 1)
        .map(|a| SEND_RETRY.resolve_delay(a, None).as_millis() as u64)
        .sum();
    assert_eq!(total, 13_000, "1+2+4+6 秒");
}
