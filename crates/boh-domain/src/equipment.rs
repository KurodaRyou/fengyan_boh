//! Equipment commands and the frozen MASTER_DATA_CHANGED@1 payload.

use serde::{Deserialize, Serialize};

use crate::{AggregateId, CommandId, DomainError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EquipmentType {
    Fridge,
    Freezer,
    BlastFreezer,
    Oven,
    Proofer,
    Mixer,
    Other,
}

impl EquipmentType {
    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "FRIDGE" => Ok(Self::Fridge),
            "FREEZER" => Ok(Self::Freezer),
            "BLAST_FREEZER" => Ok(Self::BlastFreezer),
            "OVEN" => Ok(Self::Oven),
            "PROOFER" => Ok(Self::Proofer),
            "MIXER" => Ok(Self::Mixer),
            "OTHER" => Ok(Self::Other),
            _ => Err(DomainError::InvalidField("equipment_type")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentSnapshot {
    pub code: String,
    pub name: String,
    pub equipment_type: EquipmentType,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateEquipment {
    pub command_id: CommandId,
    pub code: String,
    pub name: String,
    pub equipment_type: EquipmentType,
    pub active: bool,
}

impl CreateEquipment {
    pub fn validate(&self) -> Result<(), DomainError> {
        text(&self.code, "code")?;
        text(&self.name, "name")
    }

    pub fn snapshot(&self) -> EquipmentSnapshot {
        EquipmentSnapshot {
            code: self.code.clone(),
            name: self.name.clone(),
            equipment_type: self.equipment_type,
            active: self.active,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateEquipment {
    pub command_id: CommandId,
    pub base_revision: i64,
    pub name: String,
    pub equipment_type: EquipmentType,
    pub active: bool,
}

impl UpdateEquipment {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.base_revision <= 0 {
            return Err(DomainError::InvalidField("base_revision"));
        }
        text(&self.name, "name")
    }

    pub fn snapshot(&self, code: String) -> EquipmentSnapshot {
        EquipmentSnapshot {
            code,
            name: self.name.clone(),
            equipment_type: self.equipment_type,
            active: self.active,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Equipment {
    pub equipment_id: AggregateId,
    pub code: String,
    pub name: String,
    pub equipment_type: EquipmentType,
    pub active: bool,
    pub revision: i64,
}

impl Equipment {
    pub fn snapshot(&self) -> EquipmentSnapshot {
        EquipmentSnapshot {
            code: self.code.clone(),
            name: self.name.clone(),
            equipment_type: self.equipment_type,
            active: self.active,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum EquipmentEntity {
    #[serde(rename = "EQUIPMENT")]
    Equipment,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MasterDataSource {
    #[serde(rename = "LOCAL")]
    Local,
    #[serde(rename = "HQ_PACKAGE")]
    HqPackage,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EquipmentChanged {
    pub entity: EquipmentEntity,
    pub source: MasterDataSource,
    pub snapshot: EquipmentSnapshot,
}

fn text(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() || value.trim() != value {
        Err(DomainError::InvalidField(field))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_validation_preserves_interior_text() {
        for value in ["", " x", "x\t", "\u{3000}x", "x\n"] {
            assert!(text(value, "name").is_err());
        }
        assert!(text("后厨  冷藏柜", "name").is_ok());
    }
}
