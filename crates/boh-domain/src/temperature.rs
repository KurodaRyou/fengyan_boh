//! Temperature commands, query parameters and the frozen TEMPERATURE_LOGGED@1 payload.

use serde::{Deserialize, Serialize};

use crate::{AggregateId, CommandId, DomainError, UnixMillis};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogTemperature {
    pub command_id: CommandId,
    pub equipment_id: AggregateId,
    pub celsius_x10: i64,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::present"
    )]
    pub note: Option<String>,
    pub captured_at: UnixMillis,
    pub sent_at: UnixMillis,
}

impl LogTemperature {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_reading(self.celsius_x10, self.note.as_deref())
    }
}

// Field order is part of the published payload's serialized form.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TemperatureLogged {
    pub equipment_id: AggregateId,
    pub celsius_x10: i64,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub note: Option<String>,
}
object_serde!(TemperatureLogged);

impl TemperatureLogged {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_reading(self.celsius_x10, self.note.as_deref())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemperatureQuery {
    pub business_date: String,
    pub equipment_id: Option<AggregateId>,
}

impl TemperatureQuery {
    pub fn validate(&self) -> Result<(), DomainError> {
        let invalid = || DomainError::InvalidField("business_date");
        let bytes = self.business_date.as_bytes();
        if bytes.len() != 10
            || bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes
                .iter()
                .enumerate()
                .any(|(i, byte)| i != 4 && i != 7 && !byte.is_ascii_digit())
        {
            return Err(invalid());
        }
        self.business_date
            .parse::<jiff::civil::Date>()
            .map_err(|_| invalid())?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TemperatureReading {
    pub temperature_reading_id: AggregateId,
    pub equipment_id: AggregateId,
    pub celsius_x10: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub business_date: String,
    pub occurred_at: UnixMillis,
    pub recorded_at: UnixMillis,
    pub actor_id: AggregateId,
    pub device_id: AggregateId,
}

fn validate_reading(celsius_x10: i64, note: Option<&str>) -> Result<(), DomainError> {
    if !(-500..=5000).contains(&celsius_x10) {
        return Err(DomainError::InvalidField("celsius_x10"));
    }
    if let Some(note) = note
        && (note.is_empty() || note.trim() != note || note.chars().count() > 200)
    {
        return Err(DomainError::InvalidField("note"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_dates_require_canonical_real_calendar_dates() {
        for date in ["0000-02-29", "2024-02-29", "9999-12-31"] {
            let query = TemperatureQuery {
                business_date: date.into(),
                equipment_id: None,
            };
            assert!(query.validate().is_ok(), "{date}");
        }
        for date in [
            "2026-02-29",
            "+2026-10-06",
            "2026-10-06T00:00:00",
            "２０２６-10-06",
            "2026-10-06 ",
        ] {
            let query = TemperatureQuery {
                business_date: date.into(),
                equipment_id: None,
            };
            assert!(query.validate().is_err(), "{date}");
        }
    }
}
