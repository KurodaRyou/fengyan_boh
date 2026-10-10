//! Remaining master data commands and frozen full snapshots.

use serde::{Deserialize, Serialize};

use crate::equipment::MasterDataSource;
use crate::{AggregateId, CommandId, DomainError};

fn text(value: &str, field: &'static str) -> Result<(), DomainError> {
    if value.is_empty() || value.trim() != value {
        Err(DomainError::InvalidField(field))
    } else {
        Ok(())
    }
}

fn positive(value: i64, field: &'static str) -> Result<(), DomainError> {
    if value <= 0 {
        Err(DomainError::InvalidField(field))
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BaseUnit {
    #[serde(rename = "g")]
    G,
    #[serde(rename = "ml")]
    Ml,
    #[serde(rename = "pcs")]
    Pcs,
}

impl BaseUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::G => "g",
            Self::Ml => "ml",
            Self::Pcs => "pcs",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "g" => Ok(Self::G),
            "ml" => Ok(Self::Ml),
            "pcs" => Ok(Self::Pcs),
            _ => Err(DomainError::InvalidField("base_unit")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ItemCategory {
    Raw,
    Semi,
    Finished,
}

impl ItemCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "RAW",
            Self::Semi => "SEMI",
            Self::Finished => "FINISHED",
        }
    }

    pub fn parse(value: &str) -> Result<Self, DomainError> {
        match value {
            "RAW" => Ok(Self::Raw),
            "SEMI" => Ok(Self::Semi),
            "FINISHED" => Ok(Self::Finished),
            _ => Err(DomainError::InvalidField("category")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemUnit {
    pub unit_code: String,
    pub base_qty_per_unit: i64,
}

fn item_fields(
    name: &str,
    shelf_life: Option<i64>,
    units: &[ItemUnit],
    base_unit: Option<BaseUnit>,
) -> Result<(), DomainError> {
    text(name, "name")?;
    if let Some(value) = shelf_life {
        positive(value, "default_shelf_life_ms")?;
    }
    let mut previous: Option<&str> = None;
    for unit in units {
        text(&unit.unit_code, "unit_code")?;
        positive(unit.base_qty_per_unit, "base_qty_per_unit")?;
        if base_unit.is_some_and(|base| base.as_str() == unit.unit_code)
            || previous.is_some_and(|last| last >= unit.unit_code.as_str())
        {
            return Err(DomainError::InvalidField("units"));
        }
        previous = Some(&unit.unit_code);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemSnapshot {
    pub code: String,
    pub name: String,
    pub base_unit: BaseUnit,
    pub category: ItemCategory,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_shelf_life_ms: Option<i64>,
    pub units: Vec<ItemUnit>,
    pub active: bool,
}

impl ItemSnapshot {
    pub fn validate(&self) -> Result<(), DomainError> {
        crate::lot::validate_item_code(&self.code)?;
        item_fields(
            &self.name,
            self.default_shelf_life_ms,
            &self.units,
            Some(self.base_unit),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateItem {
    pub command_id: CommandId,
    pub code: String,
    pub name: String,
    pub base_unit: BaseUnit,
    pub category: ItemCategory,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_shelf_life_ms: Option<i64>,
    pub units: Vec<ItemUnit>,
    pub active: bool,
}

impl CreateItem {
    pub fn snapshot(&self) -> ItemSnapshot {
        ItemSnapshot {
            code: self.code.clone(),
            name: self.name.clone(),
            base_unit: self.base_unit,
            category: self.category,
            default_shelf_life_ms: self.default_shelf_life_ms,
            units: self.units.clone(),
            active: self.active,
        }
    }
    pub fn validate(&self) -> Result<(), DomainError> {
        self.snapshot().validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateItem {
    pub command_id: CommandId,
    pub base_revision: i64,
    pub name: String,
    pub category: ItemCategory,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_shelf_life_ms: Option<i64>,
    pub units: Vec<ItemUnit>,
    pub active: bool,
}

impl UpdateItem {
    pub fn validate(&self) -> Result<(), DomainError> {
        positive(self.base_revision, "base_revision")?;
        // The immutable base unit is loaded inside the command transaction.
        item_fields(&self.name, self.default_shelf_life_ms, &self.units, None)
    }
    pub fn snapshot(&self, current: &ItemSnapshot) -> ItemSnapshot {
        ItemSnapshot {
            code: current.code.clone(),
            name: self.name.clone(),
            base_unit: current.base_unit,
            category: self.category,
            default_shelf_life_ms: self.default_shelf_life_ms,
            units: self.units.clone(),
            active: self.active,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeLine {
    pub item_id: AggregateId,
    pub qty_per_batch: i64,
}

fn recipe_lines(output: i64, lines: &[RecipeLine]) -> Result<(), DomainError> {
    positive(output, "output_qty_per_batch")?;
    if lines.is_empty() {
        return Err(DomainError::InvalidField("lines"));
    }
    let mut ids = std::collections::BTreeMap::new();
    for line in lines {
        positive(line.qty_per_batch, "qty_per_batch")?;
        if ids.insert(line.item_id, ()).is_some() {
            return Err(DomainError::InvalidField("lines"));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeVersion {
    pub version: i64,
    pub output_qty_per_batch: i64,
    pub lines: Vec<RecipeLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeSnapshot {
    pub code: String,
    pub name: String,
    pub output_item_id: AggregateId,
    pub versions: Vec<RecipeVersion>,
    pub active: bool,
}

impl RecipeSnapshot {
    pub fn validate(&self) -> Result<(), DomainError> {
        text(&self.code, "code")?;
        text(&self.name, "name")?;
        if self.versions.is_empty() {
            return Err(DomainError::InvalidField("versions"));
        }
        for (index, version) in self.versions.iter().enumerate() {
            let expected = i64::try_from(index)
                .ok()
                .and_then(|n| n.checked_add(1))
                .ok_or(DomainError::InvalidField("versions"))?;
            if version.version != expected {
                return Err(DomainError::InvalidField("versions"));
            }
            recipe_lines(version.output_qty_per_batch, &version.lines)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRecipe {
    pub command_id: CommandId,
    pub code: String,
    pub name: String,
    pub output_item_id: AggregateId,
    pub active: bool,
    pub output_qty_per_batch: i64,
    pub lines: Vec<RecipeLine>,
}

impl CreateRecipe {
    pub fn validate(&self) -> Result<(), DomainError> {
        text(&self.code, "code")?;
        text(&self.name, "name")?;
        recipe_lines(self.output_qty_per_batch, &self.lines)
    }
    pub fn snapshot(&self) -> RecipeSnapshot {
        RecipeSnapshot {
            code: self.code.clone(),
            name: self.name.clone(),
            output_item_id: self.output_item_id,
            active: self.active,
            versions: vec![RecipeVersion {
                version: 1,
                output_qty_per_batch: self.output_qty_per_batch,
                lines: self.lines.clone(),
            }],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRecipe {
    pub command_id: CommandId,
    pub base_revision: i64,
    pub name: String,
    pub active: bool,
}

impl UpdateRecipe {
    pub fn validate(&self) -> Result<(), DomainError> {
        positive(self.base_revision, "base_revision")?;
        text(&self.name, "name")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddRecipeVersion {
    pub command_id: CommandId,
    pub base_revision: i64,
    pub output_qty_per_batch: i64,
    pub lines: Vec<RecipeLine>,
}

impl AddRecipeVersion {
    pub fn validate(&self) -> Result<(), DomainError> {
        positive(self.base_revision, "base_revision")?;
        recipe_lines(self.output_qty_per_batch, &self.lines)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupplierSnapshot {
    pub code: String,
    pub name: String,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub contact_phone: Option<String>,
    pub active: bool,
}

impl SupplierSnapshot {
    pub fn validate(&self) -> Result<(), DomainError> {
        text(&self.code, "code")?;
        text(&self.name, "name")?;
        if let Some(phone) = &self.contact_phone {
            text(phone, "contact_phone")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateSupplier {
    pub command_id: CommandId,
    pub code: String,
    pub name: String,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub contact_phone: Option<String>,
    pub active: bool,
}

impl CreateSupplier {
    pub fn snapshot(&self) -> SupplierSnapshot {
        SupplierSnapshot {
            code: self.code.clone(),
            name: self.name.clone(),
            contact_phone: self.contact_phone.clone(),
            active: self.active,
        }
    }
    pub fn validate(&self) -> Result<(), DomainError> {
        self.snapshot().validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSupplier {
    pub command_id: CommandId,
    pub base_revision: i64,
    pub name: String,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub contact_phone: Option<String>,
    pub active: bool,
}

impl UpdateSupplier {
    pub fn snapshot(&self, current: &SupplierSnapshot) -> SupplierSnapshot {
        SupplierSnapshot {
            code: current.code.clone(),
            name: self.name.clone(),
            contact_phone: self.contact_phone.clone(),
            active: self.active,
        }
    }
    pub fn validate(&self) -> Result<(), DomainError> {
        positive(self.base_revision, "base_revision")?;
        text(&self.name, "name")?;
        if let Some(phone) = &self.contact_phone {
            text(phone, "contact_phone")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WasteReasonSnapshot {
    pub code: String,
    pub name: String,
    pub active: bool,
}

impl WasteReasonSnapshot {
    pub fn validate(&self) -> Result<(), DomainError> {
        text(&self.code, "code")?;
        text(&self.name, "name")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateWasteReason {
    pub command_id: CommandId,
    pub code: String,
    pub name: String,
    pub active: bool,
}

impl CreateWasteReason {
    pub fn snapshot(&self) -> WasteReasonSnapshot {
        WasteReasonSnapshot {
            code: self.code.clone(),
            name: self.name.clone(),
            active: self.active,
        }
    }
    pub fn validate(&self) -> Result<(), DomainError> {
        self.snapshot().validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateWasteReason {
    pub command_id: CommandId,
    pub base_revision: i64,
    pub name: String,
    pub active: bool,
}

impl UpdateWasteReason {
    pub fn snapshot(&self, current: &WasteReasonSnapshot) -> WasteReasonSnapshot {
        WasteReasonSnapshot {
            code: current.code.clone(),
            name: self.name.clone(),
            active: self.active,
        }
    }
    pub fn validate(&self) -> Result<(), DomainError> {
        positive(self.base_revision, "base_revision")?;
        text(&self.name, "name")
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(
    tag = "entity",
    rename_all = "SCREAMING_SNAKE_CASE",
    deny_unknown_fields
)]
pub enum MasterDataChanged {
    Item {
        source: MasterDataSource,
        snapshot: ItemSnapshot,
    },
    Recipe {
        source: MasterDataSource,
        snapshot: RecipeSnapshot,
    },
    Supplier {
        source: MasterDataSource,
        snapshot: SupplierSnapshot,
    },
    WasteReason {
        source: MasterDataSource,
        snapshot: WasteReasonSnapshot,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Item {
    pub item_id: AggregateId,
    #[serde(flatten)]
    pub snapshot: ItemSnapshot,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Recipe {
    pub recipe_id: AggregateId,
    #[serde(flatten)]
    pub snapshot: RecipeSnapshot,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Supplier {
    pub supplier_id: AggregateId,
    #[serde(flatten)]
    pub snapshot: SupplierSnapshot,
    pub revision: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct WasteReason {
    pub waste_reason_id: AggregateId,
    #[serde(flatten)]
    pub snapshot: WasteReasonSnapshot,
    pub revision: i64,
}
