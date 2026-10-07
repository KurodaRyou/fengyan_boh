//! 锁定测试：闭店备份时刻的解析与下一次触发时刻。
//! 规则见 AGENTS.md「备份」触发，接口见 docs/interfaces.md「闭店备份时刻」。

use boh_domain::UnixMillis;
use boh_domain::time::{
    ConfigError, TimeError, next_closing_backup, parse_closing_backup_time, parse_timezone,
};

/// 2026-10-06 00:00 +08:00（Asia/Shanghai）。
const SH_DAY: i64 = 1_791_216_000_000;
const MINUTE: i64 = 60_000;
const HOUR: i64 = 3_600_000;

#[allow(clippy::unwrap_used)] // 测试夹具：用例只传合法的时区和时刻。
fn next(after: i64, timezone: &str, time: &str) -> Result<UnixMillis, TimeError> {
    next_closing_backup(
        UnixMillis(after),
        &parse_timezone(timezone).unwrap(),
        parse_closing_backup_time(time).unwrap(),
    )
}

// 「closing_backup_time：'HH:MM'，格式非法拒绝启动」：规则同 business_day_cutoff，Display 原样输出。
#[test]
fn closing_backup_time_accepts_only_hh_mm() {
    for value in ["00:00", "04:30", "23:30", "23:59"] {
        assert_eq!(parse_closing_backup_time(value).unwrap().to_string(), value);
    }
    for value in [
        "",
        "4:30",
        "04:3",
        "24:00",
        "23:60",
        "23:30:00",
        " 23:30",
        "23:30 ",
        "+1:30",
        "２３:３０",
        "23-30",
        "2330",
    ] {
        assert_eq!(
            parse_closing_backup_time(value),
            Err(ConfigError::InvalidClosingBackupTime(value.to_owned())),
            "{value:?}"
        );
    }
}

// 下一次闭店备份是严格晚于 after 的第一个「当地日期 + 时刻」。
#[test]
fn next_closing_backup_is_the_first_local_time_strictly_after() {
    let today = SH_DAY + 23 * HOUR + 30 * MINUTE; // 2026-10-06 23:30 +08:00
    let tomorrow = today + 24 * HOUR;
    for (after, expected) in [
        (SH_DAY + 12 * HOUR, today),
        (today - 1, today),
        (today, tomorrow),
        (today + 1, tomorrow),
        (SH_DAY - 1, today), // 2026-10-05 23:59:59.999 本地
    ] {
        assert_eq!(
            next(after, "Asia/Shanghai", "23:30"),
            Ok(UnixMillis(expected)),
            "{after}"
        );
    }
    assert_eq!(
        next(SH_DAY, "Asia/Shanghai", "00:00"),
        Ok(UnixMillis(SH_DAY + 24 * HOUR))
    );
}

// 夏令时跳过的当地时刻顺延跳过的时长：America/New_York 2026-03-08 02:00 EST 跳到 03:00 EDT，02:30 按 03:30 EDT 触发。
#[test]
fn skipped_local_time_moves_forward_by_the_gap() {
    let midnight = 1_772_946_000_000; // 2026-03-08 00:00 -05:00
    let shifted = 1_772_955_000_000; // 2026-03-08 03:30 -04:00 = 07:30Z
    let next_day = 1_773_037_800_000; // 2026-03-09 02:30 -04:00
    assert_eq!(
        next(midnight, "America/New_York", "02:30"),
        Ok(UnixMillis(shifted))
    );
    assert_eq!(
        next(shifted, "America/New_York", "02:30"),
        Ok(UnixMillis(next_day))
    );
}

// 重复的当地时刻只在第一次触发：2026-11-01 02:00 EDT 退回 01:00 EST，01:30 出现两次，只取 01:30 EDT；
// 之后的下一次是 11-02 01:30 EST，而不是当天第二个 01:30。
#[test]
fn repeated_local_time_fires_only_on_its_first_occurrence() {
    let midnight = 1_793_505_600_000; // 2026-11-01 00:00 -04:00
    let first = 1_793_511_000_000; // 2026-11-01 01:30 -04:00 = 05:30Z
    let second = 1_793_514_600_000; // 2026-11-01 01:30 -05:00 = 06:30Z
    let next_day = 1_793_601_000_000; // 2026-11-02 01:30 -05:00
    assert_eq!(
        next(midnight, "America/New_York", "01:30"),
        Ok(UnixMillis(first))
    );
    for after in [first, second - 1, second] {
        assert_eq!(
            next(after, "America/New_York", "01:30"),
            Ok(UnixMillis(next_day)),
            "{after}"
        );
    }
}

// 结果或换算超出可表示范围：OutOfRange，不 panic。
#[test]
fn unrepresentable_times_are_out_of_range() {
    for after in [i64::MAX, i64::MIN] {
        assert_eq!(
            next(after, "Asia/Shanghai", "23:30"),
            Err(TimeError::OutOfRange),
            "{after}"
        );
    }
}
