//! 四维数值结算与作息时间工具。
//!
//! 口径（v1.1 §6）：
//! - 饱食 −4/h
//! - 精力：清醒 −3.5/h；睡眠时段 +7/h（不再叠加清醒衰减）
//! - 心情 −1.2/h
//! - 亲密度只增，离线不衰减
//! - 离线结算上限 12h

use chrono::{Datelike, Local, TimeZone, Timelike};

use crate::model::PetStats;

/// 12 小时。
pub const OFFLINE_CAP_MS: i64 = 12 * 60 * 60 * 1000;

pub fn clamp_stat(value: f64) -> f64 {
    if value.is_nan() {
        return 0.0;
    }
    value.clamp(0.0, 100.0)
}

pub fn cap_elapsed_ms(raw: i64) -> i64 {
    raw.clamp(0, OFFLINE_CAP_MS)
}

/// 按小时结算一段时长。`sleep_fraction` 是这段时长中处于睡眠作息的占比。
pub fn settle_stats(stats: &mut PetStats, elapsed_ms: i64, sleep_fraction: f64) {
    let hours = cap_elapsed_ms(elapsed_ms) as f64 / 3_600_000.0;
    if hours <= 0.0 {
        return;
    }
    let sleep = sleep_fraction.clamp(0.0, 1.0);
    let awake = 1.0 - sleep;

    stats.satiety = clamp_stat(stats.satiety - 4.0 * hours);
    stats.energy = clamp_stat(stats.energy - 3.5 * hours * awake + 7.0 * hours * sleep);
    stats.mood = clamp_stat(stats.mood - 1.2 * hours);
    // 亲密度只增：离线/自然衰减永不低于当前值。
    stats.affinity = stats.affinity.max(0.0);
}

/* ------------------------------------------------------------------ */
/* 本地时间与作息                                                      */
/* ------------------------------------------------------------------ */

pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// `"HH:MM"` → 当日分钟数；非法值返回 None。
pub fn parse_hhmm(value: &str) -> Option<i32> {
    let (hour, minute) = value.trim().split_once(':')?;
    let hour: i32 = hour.trim().parse().ok()?;
    let minute: i32 = minute.trim().parse().ok()?;
    if !(0..24).contains(&hour) || !(0..60).contains(&minute) {
        return None;
    }
    Some(hour * 60 + minute)
}

pub fn local_minute_of_day(timestamp_ms: i64) -> i32 {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|dt| dt.hour() as i32 * 60 + dt.minute() as i32)
        .unwrap_or(0)
}

/// `YYYYMMDD`，用于「同一个作息点一天只触发一次」。
pub fn local_date_key(timestamp_ms: i64) -> i32 {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|dt| dt.year() * 10_000 + dt.month() as i32 * 100 + dt.day() as i32)
        .unwrap_or(0)
}

pub fn local_hhmm(timestamp_ms: i64) -> String {
    Local
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .map(|dt| format!("{:02}:{:02}", dt.hour(), dt.minute()))
        .unwrap_or_else(|| "00:00".to_string())
}

/// 睡眠窗口支持跨午夜，例如 23:30 → 08:30。
pub fn is_sleep_minute(minute: i32, from: i32, to: i32) -> bool {
    if from == to {
        return false;
    }
    if from < to {
        minute >= from && minute < to
    } else {
        minute >= from || minute < to
    }
}

/// 统计 `[start_ms, end_ms)` 中落在睡眠窗口内的分钟占比。
///
/// 最长只看 12h（与离线结算上限一致），逐分钟采样，跨午夜也正确。
pub fn sleep_fraction_between(start_ms: i64, end_ms: i64, sleep_from: i32, sleep_to: i32) -> f64 {
    if end_ms <= start_ms || sleep_from == sleep_to {
        return 0.0;
    }
    let capped_end = end_ms.min(start_ms + OFFLINE_CAP_MS);
    let total_ms = (capped_end - start_ms) as f64;
    if total_ms <= 0.0 {
        return 0.0;
    }

    let mut sleep_ms = 0.0_f64;
    let mut cursor = start_ms;
    while cursor < capped_end {
        let next = (cursor + 60_000).min(capped_end);
        let middle = cursor + (next - cursor) / 2;
        if is_sleep_minute(local_minute_of_day(middle), sleep_from, sleep_to) {
            sleep_ms += (next - cursor) as f64;
        }
        cursor = next;
    }
    (sleep_ms / total_ms).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_settlement_caps_at_12h() {
        let mut stats = PetStats::default();
        // 传给结算 24h，实际只按 12h 算。
        settle_stats(&mut stats, 24 * 3_600_000, 0.0);

        assert!(
            (stats.satiety - 32.0).abs() < 1e-9,
            "satiety={}",
            stats.satiety
        );
        assert!(
            (stats.energy - 38.0).abs() < 1e-9,
            "energy={}",
            stats.energy
        );
        assert!((stats.mood - 65.6).abs() < 1e-9, "mood={}", stats.mood);
        assert!((stats.affinity - 0.0).abs() < 1e-9);
    }

    #[test]
    fn offline_sleep_recovers_energy_and_clamps() {
        let mut stats = PetStats::default();
        settle_stats(&mut stats, 12 * 3_600_000, 1.0);
        assert!(
            (stats.energy - 100.0).abs() < 1e-9,
            "energy={}",
            stats.energy
        );
        assert!((stats.satiety - 32.0).abs() < 1e-9);
    }

    #[test]
    fn affinity_never_decreases() {
        let mut stats = PetStats {
            affinity: 7.5,
            ..PetStats::default()
        };
        settle_stats(&mut stats, 12 * 3_600_000, 0.0);
        assert!((stats.affinity - 7.5).abs() < 1e-9);
    }

    #[test]
    fn sleep_window_crosses_midnight() {
        assert!(is_sleep_minute(23 * 60 + 45, 23 * 60 + 30, 8 * 60 + 30));
        assert!(is_sleep_minute(2 * 60, 23 * 60 + 30, 8 * 60 + 30));
        assert!(!is_sleep_minute(12 * 60, 23 * 60 + 30, 8 * 60 + 30));
        assert!(!is_sleep_minute(8 * 60 + 30, 23 * 60 + 30, 8 * 60 + 30));
    }

    #[test]
    fn sleep_fraction_across_midnight_is_nine_tenths() {
        let start = Local
            .with_ymd_and_hms(2026, 11, 22, 23, 0, 0)
            .single()
            .expect("local 23:00")
            .timestamp_millis();
        let end = Local
            .with_ymd_and_hms(2026, 11, 23, 9, 0, 0)
            .single()
            .expect("local 09:00")
            .timestamp_millis();
        let fraction = sleep_fraction_between(start, end, 23 * 60 + 30, 8 * 60 + 30);
        assert!((fraction - 0.9).abs() < 0.01, "fraction={fraction}");
    }
}
