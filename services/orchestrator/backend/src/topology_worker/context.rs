//! Background context responsibilities.
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

pub(super) const CONTROL_PLANE_NODE_ID: &str = "control-plane";

pub(super) fn bounded_detail(detail: &str) -> String {
    const MAX_DETAIL_BYTES: usize = 512;
    let mut bounded = String::with_capacity(detail.len().min(MAX_DETAIL_BYTES));
    for character in detail.chars().filter(|character| !character.is_control()) {
        if bounded.len() + character.len_utf8() > MAX_DETAIL_BYTES {
            break;
        }
        bounded.push(character);
    }
    bounded
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub(super) fn now_marker() -> String {
    format!("unix-ms:{}", now_ms())
}
