//! Converts stored UTC milliseconds to and from the configured display time zone.

use chrono::{DateTime, LocalResult, NaiveDateTime, TimeZone};
use chrono_tz::Tz;

const INPUT_FORMAT: &str = "%Y-%m-%dT%H:%M";

fn local(ms: i64, tz: Tz) -> Option<DateTime<Tz>> {
    DateTime::from_timestamp_millis(ms).map(|utc| utc.with_timezone(&tz))
}

/// `2026-10-24 13:05`
pub fn display(ms: i64, tz: Tz) -> String {
    local(ms, tz).map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M").to_string())
}

/// Value for `<input type="datetime-local">`.
pub fn input_value(ms: i64, tz: Tz) -> String {
    local(ms, tz).map_or_else(String::new, |t| t.format(INPUT_FORMAT).to_string())
}

/// Parses a `datetime-local` value; an empty string means "not set".
pub fn parse_input(value: &str, tz: Tz) -> Result<Option<i64>, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let naive = NaiveDateTime::parse_from_str(value, INPUT_FORMAT)
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"))
        .map_err(|_| "時間格式不正確")?;
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => Ok(Some(t.timestamp_millis())),
        LocalResult::None => Err("這個時間在所選時區不存在"),
    }
}

fn amount(diff_ms: i64) -> String {
    let abs = (diff_ms / 60_000).abs();
    if abs < 60 {
        format!("{} 分鐘", abs.max(1))
    } else if abs < 24 * 60 {
        format!("{} 小時", abs / 60)
    } else {
        format!("{} 天", abs / (24 * 60))
    }
}

/// Short human description of how far `target` is from `now`, e.g. `3 小時後` / `2 天前`.
pub fn relative(target: i64, now: i64) -> String {
    if target >= now {
        format!("{}後", amount(target - now))
    } else {
        format!("{}前", amount(target - now))
    }
}

/// `3 小時後到期` or `已逾期 2 天`.
pub fn due_label(due: i64, now: i64) -> String {
    if due >= now {
        format!("{}後到期", amount(due - now))
    } else {
        format!("已逾期 {}", amount(due - now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAIPEI: Tz = chrono_tz::Asia::Taipei;

    #[test]
    fn taipei_round_trip() {
        let ms = parse_input("2026-10-24T13:05", TAIPEI).unwrap().unwrap();
        // 13:05 in Taipei (UTC+8) is 05:05 UTC.
        assert_eq!(ms, 1_792_818_300_000);
        assert_eq!(display(ms, TAIPEI), "2026-10-24 13:05");
        assert_eq!(input_value(ms, TAIPEI), "2026-10-24T13:05");
    }

    #[test]
    fn empty_and_invalid_input() {
        assert_eq!(parse_input("  ", TAIPEI), Ok(None));
        assert!(parse_input("tomorrow", TAIPEI).is_err());
    }

    #[test]
    fn relative_wording() {
        assert_eq!(relative(3 * 3_600_000, 0), "3 小時後");
        assert_eq!(relative(0, 2 * 86_400_000), "2 天前");
        assert_eq!(relative(10_000, 0), "1 分鐘後");
        assert_eq!(due_label(0, 2 * 86_400_000), "已逾期 2 天");
        assert_eq!(due_label(90 * 60_000, 0), "1 小時後到期");
    }
}
