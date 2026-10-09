//! Human-readable lot identity. Parsing and construction never consult I/O.

use std::fmt;

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::DomainError;
use crate::master_data::ItemCategory;

pub fn validate_item_code(code: &str) -> Result<(), DomainError> {
    if code.is_empty()
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(DomainError::InvalidField("code"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LotId {
    text: String,
    date: Date,
    serial: i64,
}

impl LotId {
    pub fn from_parts(
        category: ItemCategory,
        code: &str,
        date: Date,
        serial: i64,
    ) -> Result<Self, DomainError> {
        validate_item_code(code)?;
        if !(0..=9999).contains(&date.year()) || !(1..=999).contains(&serial) {
            return Err(DomainError::InvalidField("lot_id"));
        }
        Ok(Self {
            text: format!(
                "{}-{code}-{:04}{:02}{:02}-{serial:03}",
                category.as_str(),
                date.year(),
                date.month(),
                date.day(),
            ),
            date,
            serial,
        })
    }

    pub fn parse(text: &str) -> Result<Self, DomainError> {
        let invalid = || DomainError::InvalidField("lot_id");
        let mut parts = text.split('-');
        let category =
            ItemCategory::parse(parts.next().ok_or_else(invalid)?).map_err(|_| invalid())?;
        let code = parts.next().ok_or_else(invalid)?;
        validate_item_code(code).map_err(|_| invalid())?;
        let date = parts.next().ok_or_else(invalid)?;
        let serial = parts.next().ok_or_else(invalid)?;
        if parts.next().is_some()
            || date.len() != 8
            || !date.bytes().all(|byte| byte.is_ascii_digit())
            || serial.len() != 3
            || !serial.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid());
        }
        // The length and ASCII checks above make these byte boundaries safe.
        let date = format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..])
            .parse::<Date>()
            .map_err(|_| invalid())?;
        let serial = serial.parse::<i64>().map_err(|_| invalid())?;
        Self::from_parts(category, code, date, serial)
    }

    pub fn date(&self) -> Date {
        self.date
    }

    pub fn serial(&self) -> i64 {
        self.serial
    }
}

impl TryFrom<String> for LotId {
    type Error = DomainError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}

impl From<LotId> for String {
    fn from(id: LotId) -> Self {
        id.text
    }
}

impl fmt::Display for LotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_calendar_and_serial_boundaries_and_serializes_as_text() {
        for text in [
            "RAW-_-00000229-001",
            "SEMI-A_1-20280229-999",
            "FINISHED-0-99991231-999",
        ] {
            let id = LotId::parse(text).unwrap();
            assert_eq!(id.to_string(), text);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(serde_json::from_str::<LotId>(&json).unwrap(), id);
        }
        for text in [
            "RAW-A-20270229-001",
            "RAW-A-20261006-000",
            "RAW-A-20261006-1000",
            "RAW-A-２０２６1006-001",
            "RAW-é-20261006-001",
            "RAW-A-20261006-+01",
            "RAW-A-20261006-001-",
        ] {
            assert!(LotId::parse(text).is_err(), "{text}");
        }
    }
}
