//! 锁定测试：相对校准与营业日。规则见 docs/domain.md「时间」，接口见 docs/interfaces.md。

use boh_domain::UnixMillis;
use boh_domain::time::{
    Calibrated, CaptureTimes, ConfigError, TimeError, business_date, calibrate,
    parse_business_day_cutoff, parse_timezone,
};

const MS: i64 = 1;
const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const LIMIT: i64 = 72 * HOUR;

// 校准只看差值：平板时刻与门店时刻都取 2026-10-06（+08:00）当天，平板时钟比门店慢 7 分钟。
const DAY: i64 = 1_791_216_000_000; // 2026-10-06 00:00:00 +08:00
const T08_40: UnixMillis = UnixMillis(DAY + 8 * HOUR + 40 * MINUTE);
const T08_47: UnixMillis = UnixMillis(DAY + 8 * HOUR + 47 * MINUTE);
const T09_00: UnixMillis = UnixMillis(DAY + 9 * HOUR);
const T09_05: UnixMillis = UnixMillis(DAY + 9 * HOUR + 5 * MINUTE);
const T09_07: UnixMillis = UnixMillis(DAY + 9 * HOUR + 7 * MINUTE);
const T09_10: UnixMillis = UnixMillis(DAY + 9 * HOUR + 10 * MINUTE);
const T09_17: UnixMillis = UnixMillis(DAY + 9 * HOUR + 17 * MINUTE);
const T09_18: UnixMillis = UnixMillis(DAY + 9 * HOUR + 18 * MINUTE);
const T09_20: UnixMillis = UnixMillis(DAY + 9 * HOUR + 20 * MINUTE);
const T09_30: UnixMillis = UnixMillis(DAY + 9 * HOUR + 30 * MINUTE);
const T09_37: UnixMillis = UnixMillis(DAY + 9 * HOUR + 37 * MINUTE);
const T09_40: UnixMillis = UnixMillis(DAY + 9 * HOUR + 40 * MINUTE);
const T09_47: UnixMillis = UnixMillis(DAY + 9 * HOUR + 47 * MINUTE);

// 「相对校准」：occurred_at = recorded_at − (sent_at − captured_at)，平板时钟偏差相减后抵消。
#[test]
fn lag_is_subtracted_from_recorded_at() {
    let times = CaptureTimes {
        captured_at: T09_10,
        sent_at: T09_40,
        started_captured_at: None,
    };
    assert_eq!(
        calibrate(times, T09_47),
        Ok(Calibrated {
            occurred_at: T09_17,
            started_at: None,
            capture_time_adjusted: false,
        })
    );
}

// 「lag < 0：按 0 处理」：lag = 0 不是负数，不返回警告。
#[test]
fn zero_lag_is_not_adjusted() {
    let times = CaptureTimes {
        captured_at: T09_40,
        sent_at: T09_40,
        started_captured_at: None,
    };
    assert_eq!(
        calibrate(times, T09_47),
        Ok(Calibrated {
            occurred_at: T09_47,
            started_at: None,
            capture_time_adjusted: false,
        })
    );
}

// 「lag < 0：按 0 处理，返回警告 CAPTURE_TIME_ADJUSTED」，含 −1ms 的边界。
#[test]
fn negative_lag_counts_as_zero_with_warning() {
    for captured_at in [UnixMillis(T09_00.0 + MS), T09_10] {
        let times = CaptureTimes {
            captured_at,
            sent_at: T09_00,
            started_captured_at: None,
        };
        assert_eq!(
            calibrate(times, T09_20),
            Ok(Calibrated {
                occurred_at: T09_20,
                started_at: None,
                capture_time_adjusted: true,
            })
        );
    }
}

// 「lag > 72h：400 CAPTURE_TOO_OLD」：恰好 72h 照常换算。
#[test]
fn lag_of_exactly_72_hours_is_accepted() {
    let times = CaptureTimes {
        captured_at: UnixMillis(T09_40.0 - LIMIT),
        sent_at: T09_40,
        started_captured_at: None,
    };
    assert_eq!(
        calibrate(times, T09_47),
        Ok(Calibrated {
            occurred_at: UnixMillis(T09_47.0 - LIMIT),
            started_at: None,
            capture_time_adjusted: false,
        })
    );
}

// 「lag > 72h：400 CAPTURE_TOO_OLD」：多 1ms 即拒绝。
#[test]
fn lag_over_72_hours_is_too_old() {
    let times = CaptureTimes {
        captured_at: UnixMillis(T09_40.0 - LIMIT - MS),
        sent_at: T09_40,
        started_captured_at: None,
    };
    assert_eq!(calibrate(times, T09_47), Err(TimeError::CaptureTooOld));
}

// 「开始时间也适用 72 小时上限」：开始时间的 lag 恰好 72h 照常换算。
#[test]
fn start_lag_of_exactly_72_hours_is_accepted() {
    let times = CaptureTimes {
        captured_at: T08_40,
        sent_at: T09_40,
        started_captured_at: Some(UnixMillis(T09_40.0 - LIMIT)),
    };
    assert_eq!(
        calibrate(times, T09_47),
        Ok(Calibrated {
            occurred_at: T08_47,
            started_at: Some(UnixMillis(T09_47.0 - LIMIT)),
            capture_time_adjusted: false,
        })
    );
}

// 「开始时间也适用 72 小时上限」：完成时间只有 1h，开始时间超出 1ms 也拒绝。
#[test]
fn start_lag_over_72_hours_is_too_old() {
    let times = CaptureTimes {
        captured_at: T08_40,
        sent_at: T09_40,
        started_captured_at: Some(UnixMillis(T09_40.0 - LIMIT - MS)),
    };
    assert_eq!(calibrate(times, T09_47), Err(TimeError::CaptureTooOld));
}

// 「用同一个 sent_at、recorded_at 换算出 started_at」：两段 lag 分别换算。
#[test]
fn start_and_completion_are_calibrated_with_the_same_clock() {
    let times = CaptureTimes {
        captured_at: T09_30,
        sent_at: T09_40,
        started_captured_at: Some(T09_10),
    };
    assert_eq!(
        calibrate(times, T09_47),
        Ok(Calibrated {
            occurred_at: T09_37,
            started_at: Some(T09_17),
            capture_time_adjusted: false,
        })
    );
}

// 验收用例「生产开始晚于完成」的相对校准部分。
#[test]
fn production_start_after_completion_is_rejected() {
    let times = CaptureTimes {
        captured_at: T09_10,
        sent_at: T09_40,
        started_captured_at: Some(T09_30),
    };
    assert_eq!(
        calibrate(times, T09_47),
        Err(TimeError::InvalidProductionTime)
    );
}

// 验收用例「负 lag 不掩盖生产顺序」：两段 lag 都为负，归零前就拒绝。
#[test]
fn negative_lag_does_not_hide_production_order() {
    let times = CaptureTimes {
        captured_at: T09_10,
        sent_at: T09_00,
        started_captured_at: Some(T09_30),
    };
    assert_eq!(
        calibrate(times, T09_20),
        Err(TimeError::InvalidProductionTime)
    );
}

// 验收用例「生产开始等于完成」：时间顺序合法，照常换算。
#[test]
fn production_start_equal_to_completion_is_accepted() {
    let times = CaptureTimes {
        captured_at: T09_10,
        sent_at: T09_40,
        started_captured_at: Some(T09_10),
    };
    assert_eq!(
        calibrate(times, T09_47),
        Ok(Calibrated {
            occurred_at: T09_17,
            started_at: Some(T09_17),
            capture_time_adjusted: false,
        })
    );
}

// 「lag < 0：按 0 处理」逐段适用：只有为负的那段归零。
#[test]
fn each_negative_lag_counts_as_zero() {
    let only_completion = CaptureTimes {
        captured_at: T09_10,
        sent_at: T09_07,
        started_captured_at: Some(T09_05),
    };
    assert_eq!(
        calibrate(only_completion, T09_20),
        Ok(Calibrated {
            occurred_at: T09_20,
            started_at: Some(T09_18),
            capture_time_adjusted: true,
        })
    );
    let both = CaptureTimes {
        captured_at: T09_10,
        sent_at: T09_00,
        started_captured_at: Some(T09_05),
    };
    assert_eq!(
        calibrate(both, T09_20),
        Ok(Calibrated {
            occurred_at: T09_20,
            started_at: Some(T09_20),
            capture_time_adjusted: true,
        })
    );
}

// 「先校验 started_captured_at <= captured_at，再计算两段 lag」：顺序错误优先于 72 小时上限。
#[test]
fn production_order_is_checked_before_the_lag_limit() {
    let captured_at = UnixMillis(T09_40.0 - LIMIT - MS);
    let times = CaptureTimes {
        captured_at,
        sent_at: T09_40,
        started_captured_at: Some(UnixMillis(captured_at.0 + MINUTE)),
    };
    assert_eq!(
        calibrate(times, T09_47),
        Err(TimeError::InvalidProductionTime)
    );
}

// docs/interfaces.md：任何一步溢出都返回 OutOfRange，不 panic。
#[test]
fn arithmetic_overflow_is_out_of_range() {
    let lag_overflow = CaptureTimes {
        captured_at: UnixMillis(i64::MIN),
        sent_at: UnixMillis(i64::MAX),
        started_captured_at: None,
    };
    assert_eq!(calibrate(lag_overflow, T09_47), Err(TimeError::OutOfRange));

    let start_lag_overflow = CaptureTimes {
        captured_at: T09_10,
        sent_at: T09_40,
        started_captured_at: Some(UnixMillis(i64::MIN)),
    };
    assert_eq!(
        calibrate(start_lag_overflow, T09_47),
        Err(TimeError::OutOfRange)
    );

    let occurred_overflow = CaptureTimes {
        captured_at: T09_10,
        sent_at: T09_40,
        started_captured_at: None,
    };
    assert_eq!(
        calibrate(occurred_overflow, UnixMillis(i64::MIN)),
        Err(TimeError::OutOfRange)
    );

    // occurred_at = i64::MIN 仍可表示，started_at = i64::MIN − 1 溢出。
    let started_overflow = CaptureTimes {
        captured_at: UnixMillis(0),
        sent_at: UnixMillis(0),
        started_captured_at: Some(UnixMillis(-1)),
    };
    assert_eq!(
        calibrate(started_overflow, UnixMillis(i64::MIN)),
        Err(TimeError::OutOfRange)
    );
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
