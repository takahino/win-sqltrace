//! 時刻の表示。OS に依存しないように、地方時との差（分）は呼び出し側から受け取る（Windows では cli が得る）。

/// 表示に使う時差。`offset_minutes` は UTC に足す分（日本は +540）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeZone {
    pub offset_minutes: i32,
}

struct Civil {
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millis: u32,
}

impl TimeZone {
    fn civil(self, time_ms: i64) -> Civil {
        let local = time_ms + i64::from(self.offset_minutes) * 60_000;
        let days = local.div_euclid(86_400_000);
        let in_day = local.rem_euclid(86_400_000);
        let (year, month, day) = civil_from_days(days);
        Civil {
            year,
            month,
            day,
            hour: (in_day / 3_600_000) as u32,
            minute: (in_day / 60_000 % 60) as u32,
            second: (in_day / 1000 % 60) as u32,
            millis: (in_day % 1000) as u32,
        }
    }

    /// `2026-11-26 14:03:05.123`
    pub fn date_time(self, time_ms: i64) -> String {
        let c = self.civil(time_ms);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            c.year, c.month, c.day, c.hour, c.minute, c.second, c.millis
        )
    }

    /// `14:03:05.123`
    pub fn clock(self, time_ms: i64) -> String {
        let c = self.civil(time_ms);
        format!("{:02}:{:02}:{:02}.{:03}", c.hour, c.minute, c.second, c.millis)
    }
}

/// 1970-01-01 からの日数を年月日にする（Howard Hinnant の days_from_civil の逆）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_and_local() {
        // 2026-11-26 05:03:05.123 UTC
        let t = 1_795_669_385_123;
        assert_eq!(TimeZone::default().date_time(t), "2026-11-26 05:03:05.123");
        let japan = TimeZone { offset_minutes: 540 };
        assert_eq!(japan.date_time(t), "2026-11-26 14:03:05.123");
        assert_eq!(japan.clock(t), "14:03:05.123");
    }

    #[test]
    fn crosses_day_month_and_leap_day() {
        assert_eq!(TimeZone::default().date_time(0), "1970-01-01 00:00:00.000");
        // 2024-02-29 23:30 UTC は、日本では 2024-03-01 08:30。
        let t = 1_709_249_400_000;
        assert_eq!(TimeZone::default().date_time(t), "2024-02-29 23:30:00.000");
        assert_eq!(TimeZone { offset_minutes: 540 }.date_time(t), "2024-03-01 08:30:00.000");
        // 西の時差で前の日に戻る。
        assert_eq!(
            TimeZone { offset_minutes: -300 }.date_time(0),
            "1969-12-31 19:00:00.000"
        );
    }
}
