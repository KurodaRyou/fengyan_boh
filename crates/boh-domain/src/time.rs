//! 相对校准与门店营业日；不读取系统时间或宿主时区。

use std::fmt;

use jiff::Timestamp;
use jiff::civil::{Date, Time};
use jiff::tz::{TimeZone, TimeZoneDatabase};

use crate::UnixMillis;

const MAX_CAPTURE_LAG_MS: i64 = 259_200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureTimes {
    pub captured_at: UnixMillis,
    pub sent_at: UnixMillis,
    pub started_captured_at: Option<UnixMillis>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calibrated {
    pub occurred_at: UnixMillis,
    pub started_at: Option<UnixMillis>,
    pub capture_time_adjusted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("capture is more than 72 hours old")]
    CaptureTooOld,
    #[error("production start is after completion")]
    InvalidProductionTime,
    #[error("time is outside the supported range")]
    OutOfRange,
}

/// 按生产顺序、差值溢出、72 小时上限、负值归零、换算溢出的顺序判定。
pub fn calibrate(times: CaptureTimes, recorded_at: UnixMillis) -> Result<Calibrated, TimeError> {
    if times
        .started_captured_at
        .is_some_and(|start| start > times.captured_at)
    {
        return Err(TimeError::InvalidProductionTime);
    }
    let lag = times
        .sent_at
        .0
        .checked_sub(times.captured_at.0)
        .ok_or(TimeError::OutOfRange)?;
    let start_lag = times
        .started_captured_at
        .map(|start| {
            times
                .sent_at
                .0
                .checked_sub(start.0)
                .ok_or(TimeError::OutOfRange)
        })
        .transpose()?;
    if lag > MAX_CAPTURE_LAG_MS || start_lag.is_some_and(|lag| lag > MAX_CAPTURE_LAG_MS) {
        return Err(TimeError::CaptureTooOld);
    }
    let capture_time_adjusted = lag < 0 || start_lag.is_some_and(|lag| lag < 0);
    let lag = lag.max(0);
    let start_lag = start_lag.map(|lag| lag.max(0));
    let occurred_at = UnixMillis(
        recorded_at
            .0
            .checked_sub(lag)
            .ok_or(TimeError::OutOfRange)?,
    );
    let started_at = start_lag
        .map(|lag| {
            recorded_at
                .0
                .checked_sub(lag)
                .map(UnixMillis)
                .ok_or(TimeError::OutOfRange)
        })
        .transpose()?;
    Ok(Calibrated {
        occurred_at,
        started_at,
        capture_time_adjusted,
    })
}

#[derive(Debug, Clone)]
pub struct StoreTimeZone(TimeZone);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusinessDayCutoff(Time);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessDate(Date);

impl fmt::Display for BusinessDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid IANA time zone: {0}")]
    InvalidTimeZone(String),
    #[error("invalid business day cutoff (expected HH:MM): {0}")]
    InvalidBusinessDayCutoff(String),
}

pub fn parse_timezone(value: &str) -> Result<StoreTimeZone, ConfigError> {
    let timezone = TimeZoneDatabase::bundled()
        .get(value)
        .map_err(|_| ConfigError::InvalidTimeZone(value.to_owned()))?;
    if timezone.is_unknown() {
        return Err(ConfigError::InvalidTimeZone(value.to_owned()));
    }
    Ok(StoreTimeZone(timezone))
}

pub fn parse_business_day_cutoff(value: &str) -> Result<BusinessDayCutoff, ConfigError> {
    let invalid = || ConfigError::InvalidBusinessDayCutoff(value.to_owned());
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || ![bytes[0], bytes[1], bytes[3], bytes[4]]
            .iter()
            .all(u8::is_ascii_digit)
    {
        return Err(invalid());
    }
    let hour = value[..2].parse::<i8>().map_err(|_| invalid())?;
    let minute = value[3..].parse::<i8>().map_err(|_| invalid())?;
    Time::new(hour, minute, 0, 0)
        .map(BusinessDayCutoff)
        .map_err(|_| invalid())
}

pub fn business_date(
    occurred_at: UnixMillis,
    timezone: &StoreTimeZone,
    cutoff: BusinessDayCutoff,
) -> Result<BusinessDate, TimeError> {
    let timestamp =
        Timestamp::from_millisecond(occurred_at.0).map_err(|_| TimeError::OutOfRange)?;
    let local = timezone.0.to_datetime(timestamp);
    let date = if local.time() < cutoff.0 {
        local
            .date()
            .yesterday()
            .map_err(|_| TimeError::OutOfRange)?
    } else {
        local.date()
    };
    if !(0..=9999).contains(&date.year()) {
        return Err(TimeError::OutOfRange);
    }
    Ok(BusinessDate(date))
}
