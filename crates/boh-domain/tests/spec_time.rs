//! 锁定测试：发生时间的校验、节点校时信号与营业日。规则见 docs/domain.md「时间」，接口见 docs/interfaces.md。

use boh_domain::UnixMillis;
use boh_domain::time::{
    CaptureTimes, ConfigError, TimeError, business_date, check_capture_times, clock_signal_valid,
    parse_business_day_cutoff, parse_timezone,
};

const MS: i64 = 1;
const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
/// 「太早」：recorded_at − occurred_at 超过 72 小时。
const LIMIT: i64 = 72 * HOUR;
/// 校时信号的有效期：7 天。
const WEEK: i64 = 7 * 24 * HOUR;

const DAY: i64 = 1_791_216_000_000; // 2026-10-06 00:00:00 +08:00
/// 写事务内的节点时间。
const RECORDED: i64 = DAY + 9 * HOUR; // 09:00
/// 前端上次校时：录入前 2 小时。
const SYNCED: i64 = RECORDED - 2 * HOUR;

fn times(occurred_at: i64, started_at: Option<i64>, clock_synced_at: i64) -> CaptureTimes {
    CaptureTimes {
        occurred_at: UnixMillis(occurred_at),
        started_at: started_at.map(UnixMillis),
        clock_synced_at: UnixMillis(clock_synced_at),
    }
}

fn check(occurred_at: i64, started_at: Option<i64>, clock_synced_at: i64) -> Result<(), TimeError> {
    check_capture_times(
        times(occurred_at, started_at, clock_synced_at),
        UnixMillis(RECORDED),
    )
}

// 「发生时间的校验」：前端信号有效、occurred_at 不晚于接收时间超过 1 分钟、不早于 72 小时时照常受理；
// 节点只校验、不修改发生时间（函数不返回任何换算结果）。
#[test]
fn live_capture_within_limits_is_accepted() {
    for occurred_at in [RECORDED, RECORDED - 10 * MINUTE, RECORDED + 30_000] {
        assert_eq!(check(occurred_at, None, SYNCED), Ok(()), "{occurred_at}");
    }
}

// 「前端信号」：clock_synced_at ≤ recorded_at + 1 分钟（恰好 1 分钟照常受理，多 1 ms 拒绝）。
#[test]
fn client_sync_may_lead_the_node_by_at_most_one_minute() {
    assert_eq!(check(RECORDED, None, RECORDED + MINUTE), Ok(()));
    assert_eq!(
        check(RECORDED, None, RECORDED + MINUTE + MS),
        Err(TimeError::ClientClockNotSynced)
    );
}

// 「前端信号」：recorded_at ≤ clock_synced_at + 7 天（恰好 7 天照常受理，多 1 ms 拒绝）。
#[test]
fn client_sync_expires_after_seven_days() {
    assert_eq!(check(RECORDED, None, RECORDED - WEEK), Ok(()));
    assert_eq!(
        check(RECORDED, None, RECORDED - WEEK - MS),
        Err(TimeError::ClientClockNotSynced)
    );
}

// 「晚于接收时间」：occurred_at 晚于 recorded_at 不超过 1 分钟照常受理，超过 1 ms 即 CLIENT_CLOCK_NOT_SYNCED。
#[test]
fn occurred_at_may_lead_recorded_at_by_at_most_one_minute() {
    assert_eq!(check(RECORDED + MINUTE, None, SYNCED), Ok(()));
    assert_eq!(
        check(RECORDED + MINUTE + MS, None, SYNCED),
        Err(TimeError::ClientClockNotSynced)
    );
}

// 「太早」：recorded_at − occurred_at 恰好 72 小时照常受理，多 1 ms 返回 CAPTURE_TOO_OLD。
#[test]
fn capture_older_than_72_hours_is_too_old() {
    assert_eq!(check(RECORDED - LIMIT, None, SYNCED), Ok(()));
    assert_eq!(
        check(RECORDED - LIMIT - MS, None, SYNCED),
        Err(TimeError::CaptureTooOld)
    );
}

// 「生产另带 started_at」：started_at 同样适用 1 分钟和 72 小时的界限；验收用例「开始时间超过 72 小时」：
// occurred_at 早 1 小时、started_at 早 72 小时零 1 ms 时 CAPTURE_TOO_OLD。
#[test]
fn started_at_has_the_same_limits() {
    let occurred_at = RECORDED - HOUR;
    assert_eq!(check(occurred_at, Some(RECORDED - LIMIT), SYNCED), Ok(()));
    assert_eq!(
        check(occurred_at, Some(RECORDED - LIMIT - MS), SYNCED),
        Err(TimeError::CaptureTooOld)
    );
    // started_at 晚于接收时间超过 1 分钟时，它也晚于 occurred_at；只看 1 分钟界限时 occurred_at 也要超前。
    assert_eq!(
        check(RECORDED + MINUTE, Some(RECORDED + MINUTE), SYNCED),
        Ok(())
    );
    assert_eq!(
        check(RECORDED + MINUTE + MS, Some(RECORDED + MINUTE + MS), SYNCED),
        Err(TimeError::ClientClockNotSynced)
    );
}

// 验收用例「生产开始晚于完成」「生产开始等于完成」：started_at > occurred_at 时 INVALID_PRODUCTION_TIME，相等允许。
#[test]
fn production_start_must_not_be_after_completion() {
    let occurred_at = RECORDED - 50 * MINUTE; // 09:10 完成
    assert_eq!(check(occurred_at, Some(occurred_at), SYNCED), Ok(()));
    assert_eq!(
        check(occurred_at, Some(RECORDED - 30 * MINUTE), SYNCED), // 09:30 开始
        Err(TimeError::InvalidProductionTime)
    );
    assert_eq!(
        check(occurred_at, Some(occurred_at + MS), SYNCED),
        Err(TimeError::InvalidProductionTime)
    );
}

// docs/interfaces.md：比较按数学上的整数进行，极端值不溢出、不 panic，按差值的真实大小判定；不返回 OutOfRange。
#[test]
fn extreme_values_are_compared_without_overflow() {
    assert_eq!(check(i64::MIN, None, SYNCED), Err(TimeError::CaptureTooOld));
    assert_eq!(
        check(i64::MAX, None, SYNCED),
        Err(TimeError::ClientClockNotSynced)
    );
    assert_eq!(
        check(RECORDED, None, i64::MIN),
        Err(TimeError::ClientClockNotSynced)
    );
    assert_eq!(
        check(RECORDED, None, i64::MAX),
        Err(TimeError::ClientClockNotSynced)
    );
    assert_eq!(
        check(RECORDED - HOUR, Some(i64::MIN), SYNCED),
        Err(TimeError::CaptureTooOld)
    );
    // recorded_at 本身在边界上：差值同样按真实大小判定。
    let at_min = check_capture_times(times(i64::MIN, None, i64::MIN), UnixMillis(i64::MIN));
    assert_eq!(at_min, Ok(()));
    let at_max = check_capture_times(times(i64::MAX, None, i64::MAX), UnixMillis(i64::MAX));
    assert_eq!(at_max, Ok(()));
    assert_eq!(
        check_capture_times(times(i64::MIN, None, i64::MAX), UnixMillis(i64::MAX)),
        Err(TimeError::CaptureTooOld)
    );
    assert_eq!(
        check_capture_times(times(i64::MAX, None, i64::MAX), UnixMillis(i64::MIN)),
        Err(TimeError::ClientClockNotSynced)
    );
}

// 「校时信号」：上次校时 ≤ 现在 ≤ 上次校时 + 7 天时有效；两端都含，各差 1 ms 即无效（现在早于上次校时 = 时钟复位到过去）。
#[test]
fn clock_signal_is_valid_for_seven_days_from_the_last_sync() {
    let last = UnixMillis(SYNCED);
    for (now, valid) in [
        (SYNCED, true),
        (SYNCED + HOUR, true),
        (SYNCED + WEEK, true),
        (SYNCED + WEEK + MS, false),
        (SYNCED - MS, false),
        (SYNCED - WEEK, false),
    ] {
        assert_eq!(clock_signal_valid(last, UnixMillis(now)), valid, "{now}");
    }
}

// 节点信号不允许领先：前端的 clock_synced_at 可以比 recorded_at 晚 1 分钟，节点的上次校时晚于现在 1 ms 即无效。
#[test]
fn node_signal_has_no_lead_tolerance() {
    assert!(!clock_signal_valid(
        UnixMillis(RECORDED + MINUTE),
        UnixMillis(RECORDED)
    ));
    assert_eq!(check(RECORDED, None, RECORDED + MINUTE), Ok(()));
}

// docs/interfaces.md：极端值不溢出、不 panic。
#[test]
fn clock_signal_with_extreme_values() {
    assert!(!clock_signal_valid(
        UnixMillis(i64::MAX),
        UnixMillis(i64::MIN)
    ));
    assert!(!clock_signal_valid(
        UnixMillis(i64::MIN),
        UnixMillis(i64::MAX)
    ));
    assert!(clock_signal_valid(
        UnixMillis(i64::MAX),
        UnixMillis(i64::MAX)
    ));
    assert!(clock_signal_valid(
        UnixMillis(i64::MAX - WEEK),
        UnixMillis(i64::MAX)
    ));
    assert!(!clock_signal_valid(
        UnixMillis(i64::MAX - WEEK - MS),
        UnixMillis(i64::MAX)
    ));
    assert!(clock_signal_valid(
        UnixMillis(i64::MIN),
        UnixMillis(i64::MIN + WEEK)
    ));
}

// 「营业日」：当地时刻早于日切的算前一营业日（含跨年）；恰好等于日切算当天。
#[test]
fn business_day_starts_at_the_cutoff() {
    let shanghai = parse_timezone("Asia/Shanghai").unwrap();
    let cutoff = parse_business_day_cutoff("04:00").unwrap();
    for (occurred_at, expected) in [
        (1_791_230_399_999, "2026-10-05"), // 2026-10-06 03:59:59.999 +08:00
        (1_791_230_400_000, "2026-10-06"), // 2026-10-06 04:00:00.000 +08:00
        (1_767_211_199_999, "2025-12-31"), // 2026-01-01 03:59:59.999 +08:00
    ] {
        let date = business_date(UnixMillis(occurred_at), &shanghai, cutoff).unwrap();
        assert_eq!(date.to_string(), expected, "occurred_at = {occurred_at}");
    }
}

// 「营业日」：按门店当地日期归属，不按 UTC 日期。
#[test]
fn business_date_follows_the_local_date() {
    let shanghai = parse_timezone("Asia/Shanghai").unwrap();
    let cutoff = parse_business_day_cutoff("04:00").unwrap();
    for (occurred_at, expected) in [
        (1_791_243_000_000, "2026-10-06"), // 2026-10-05 23:30Z = 2026-10-06 07:30 +08:00
        (1_767_214_800_000, "2026-01-01"), // 2025-12-31 21:00Z = 2026-01-01 05:00 +08:00
    ] {
        let date = business_date(UnixMillis(occurred_at), &shanghai, cutoff).unwrap();
        assert_eq!(date.to_string(), expected, "occurred_at = {occurred_at}");
    }
}

// 「营业日」只按配置的 timezone 和 business_day_cutoff 计算：同一时刻换一组配置，归属随之改变。
#[test]
fn business_date_uses_the_configured_values() {
    let utc = parse_timezone("UTC").unwrap();
    let midnight = parse_business_day_cutoff("00:00").unwrap();
    let date = business_date(UnixMillis(1_791_243_000_000), &utc, midnight).unwrap();
    assert_eq!(date.to_string(), "2026-10-05"); // 2026-10-05 23:30Z
}

// docs/interfaces.md：无法表示为 'YYYY-MM-DD' 的时刻返回 OutOfRange，不 panic。
#[test]
fn unrepresentable_occurred_at_is_out_of_range() {
    let shanghai = parse_timezone("Asia/Shanghai").unwrap();
    let cutoff = parse_business_day_cutoff("04:00").unwrap();
    for occurred_at in [i64::MIN, i64::MAX] {
        assert_eq!(
            business_date(UnixMillis(occurred_at), &shanghai, cutoff),
            Err(TimeError::OutOfRange)
        );
    }
}

// docs/interfaces.md：营业日年份超出 0000–9999 时返回 OutOfRange，日切回退到前一日后同样检查。
#[test]
fn business_date_before_year_zero_is_out_of_range() {
    let utc = parse_timezone("UTC").unwrap();
    let cutoff = parse_business_day_cutoff("04:00").unwrap();
    // 0000-01-01 03:59:59.999Z 早于日切，前一营业日是 -0001-12-31。
    assert_eq!(
        business_date(UnixMillis(-62_167_204_800_001), &utc, cutoff),
        Err(TimeError::OutOfRange)
    );
}

// 配置值解析：IANA 时区名；日切为两位小时、两位分钟的 'HH:MM'。
#[test]
fn valid_config_values_are_accepted() {
    for timezone in ["Asia/Shanghai", "UTC"] {
        assert!(parse_timezone(timezone).is_ok(), "{timezone:?}");
    }
    for cutoff in ["00:00", "04:00", "23:59"] {
        assert!(parse_business_day_cutoff(cutoff).is_ok(), "{cutoff:?}");
    }
}

// 配置值非法时拒绝启动：timezone 不是 IANA 时区名，包括 jiff 保留的 Etc/Unknown。
#[test]
fn invalid_timezone_is_rejected() {
    for timezone in [
        "",
        "Asia/Shangai",
        "UTC+8",
        "+08:00",
        " Asia/Shanghai",
        "Etc/Unknown",
    ] {
        assert!(
            matches!(
                parse_timezone(timezone),
                Err(ConfigError::InvalidTimeZone(_))
            ),
            "{timezone:?}"
        );
    }
}

// 配置值非法时拒绝启动：business_day_cutoff 不是合法的 'HH:MM'。
#[test]
fn invalid_business_day_cutoff_is_rejected() {
    for cutoff in [
        "", "4:00", "04:0", "24:00", "04:60", "04:00:00", "0400", " 04:00", "04:00 ",
    ] {
        assert!(
            matches!(
                parse_business_day_cutoff(cutoff),
                Err(ConfigError::InvalidBusinessDayCutoff(_))
            ),
            "{cutoff:?}"
        );
    }
}
