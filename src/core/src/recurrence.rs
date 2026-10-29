//! Recurrence rules of a series: which local dates have an occurrence and when it starts.

use chrono::{Datelike, Days, LocalResult, NaiveDate, TimeZone};
use chrono_tz::Tz;

/// How far ahead occurrences are turned into issues.
pub const HORIZON_DAYS: u64 = 14;

pub const DATE_FORMAT: &str = "%Y-%m-%d";

/// Monday first, matching the `weekdays` bit order.
pub const WEEKDAY_LABELS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
}

impl Freq {
    pub const ALL: [Freq; 3] = [Freq::Daily, Freq::Weekly, Freq::Monthly];

    pub fn as_str(self) -> &'static str {
        match self {
            Freq::Daily => "daily",
            Freq::Weekly => "weekly",
            Freq::Monthly => "monthly",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Freq::Daily => "每天",
            Freq::Weekly => "每週",
            Freq::Monthly => "每月",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recurrence {
    pub freq: Freq,
    /// Weekly only: bit 0 = Monday … bit 6 = Sunday; never empty for weekly rules.
    pub weekdays: u8,
    /// First possible occurrence; monthly rules repeat on its day of month.
    pub start_date: NaiveDate,
    /// Last possible occurrence, inclusive.
    pub until: Option<NaiveDate>,
    /// Local start time, minutes after midnight.
    pub start_minute: u32,
    pub duration_min: i64,
}

pub fn weekday_bit(date: NaiveDate) -> u8 {
    1 << date.weekday().num_days_from_monday()
}

pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), DATE_FORMAT).ok()
}

pub fn format_date(date: NaiveDate) -> String {
    date.format(DATE_FORMAT).to_string()
}

/// The local date of `ms` in `tz`.
pub fn local_date(ms: i64, tz: Tz) -> NaiveDate {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .with_timezone(&tz)
        .date_naive()
}

/// Last date occurrences are generated up to, as of `now`.
pub fn horizon(now: i64, tz: Tz) -> NaiveDate {
    local_date(now, tz) + Days::new(HORIZON_DAYS)
}

impl Recurrence {
    pub fn matches(&self, date: NaiveDate) -> bool {
        if date < self.start_date || self.until.is_some_and(|until| date > until) {
            return false;
        }
        match self.freq {
            Freq::Daily => true,
            Freq::Weekly => self.weekdays & weekday_bit(date) != 0,
            Freq::Monthly => date.day() == self.start_date.day(),
        }
    }

    /// Unix milliseconds of the occurrence on `date`, or `None` when that local time is
    /// skipped by a daylight-saving change.
    pub fn start_ms(&self, date: NaiveDate, tz: Tz) -> Option<i64> {
        let time =
            chrono::NaiveTime::from_num_seconds_from_midnight_opt(self.start_minute * 60, 0)?;
        match tz.from_local_datetime(&date.and_time(time)) {
            LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => Some(t.timestamp_millis()),
            LocalResult::None => None,
        }
    }

    /// Occurrence dates in `from..=to` with their start times.
    pub fn occurrences(&self, from: NaiveDate, to: NaiveDate, tz: Tz) -> Vec<(NaiveDate, i64)> {
        from.iter_days()
            .take_while(|d| *d <= to)
            .filter(|d| self.matches(*d))
            .filter_map(|d| Some((d, self.start_ms(d, tz)?)))
            .collect()
    }

    /// e.g. `每週三、五 14:00–15:00`.
    pub fn describe(&self) -> String {
        let days = match self.freq {
            Freq::Daily => "每天".to_owned(),
            Freq::Weekly => {
                let names: Vec<&str> = (0..7)
                    .filter(|i| self.weekdays & (1 << i) != 0)
                    .map(|i| WEEKDAY_LABELS[i])
                    .collect();
                format!("每週{}", names.join("、"))
            }
            Freq::Monthly => format!("每月 {} 日", self.start_date.day()),
        };
        let end = i64::from(self.start_minute) + self.duration_min;
        let clock = |m: i64| format!("{:02}:{:02}", m.rem_euclid(1440) / 60, m.rem_euclid(60));
        let later = match end / 1440 {
            0 => String::new(),
            1 => "（隔天）".to_owned(),
            n => format!("（{n} 天後）"),
        };
        format!(
            "{days} {}–{}{later}",
            clock(i64::from(self.start_minute)),
            clock(end)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TZ: Tz = chrono_tz::Asia::Taipei;

    fn date(s: &str) -> NaiveDate {
        parse_date(s).unwrap()
    }

    fn weekly(weekdays: u8) -> Recurrence {
        Recurrence {
            freq: Freq::Weekly,
            weekdays,
            start_date: date("2026-10-26"),
            until: Some(date("2026-11-30")),
            start_minute: 14 * 60,
            duration_min: 60,
        }
    }

    #[test]
    fn weekly_on_wednesday() {
        // 2026-10-28 is a Wednesday.
        let rule = weekly(weekday_bit(date("2026-10-28")));
        let got = rule.occurrences(date("2026-10-20"), date("2026-11-12"), TZ);
        let days: Vec<String> = got.iter().map(|(d, _)| format_date(*d)).collect();
        assert_eq!(days, ["2026-10-28", "2026-11-04", "2026-11-11"]);
        // 14:00 in Taipei is 06:00 UTC.
        assert_eq!(got[0].1 % 86_400_000, 6 * 3_600_000);
        assert_eq!(rule.describe(), "每週三 14:00–15:00");
    }

    #[test]
    fn until_and_start_bound_the_rule() {
        let rule = weekly(0b111_1111);
        assert!(!rule.matches(date("2026-10-25")));
        assert!(rule.matches(date("2026-10-26")));
        assert!(rule.matches(date("2026-11-30")));
        assert!(!rule.matches(date("2026-12-01")));
    }

    #[test]
    fn monthly_skips_short_months() {
        let rule = Recurrence {
            freq: Freq::Monthly,
            weekdays: 0,
            start_date: date("2026-01-31"),
            until: None,
            start_minute: 9 * 60,
            duration_min: 30,
        };
        let days: Vec<String> = rule
            .occurrences(date("2026-01-01"), date("2026-04-30"), TZ)
            .iter()
            .map(|(d, _)| format_date(*d))
            .collect();
        assert_eq!(days, ["2026-01-31", "2026-03-31"]);
        assert_eq!(rule.describe(), "每月 31 日 09:00–09:30");
    }

    #[test]
    fn overnight_description() {
        let mut rule = weekly(1);
        rule.freq = Freq::Daily;
        rule.start_minute = 23 * 60;
        rule.duration_min = 120;
        assert_eq!(rule.describe(), "每天 23:00–01:00（隔天）");
    }

    #[test]
    fn skipped_local_time_has_no_occurrence() {
        // New York skips 02:00–03:00 on 2026-03-08.
        let ny = chrono_tz::America::New_York;
        let mut rule = weekly(0);
        rule.freq = Freq::Daily;
        rule.start_date = date("2026-03-07");
        rule.until = None;
        rule.start_minute = 2 * 60 + 30;
        let days: Vec<String> = rule
            .occurrences(date("2026-03-07"), date("2026-03-09"), ny)
            .iter()
            .map(|(d, _)| format_date(*d))
            .collect();
        assert_eq!(days, ["2026-03-07", "2026-03-09"]);
    }
}
