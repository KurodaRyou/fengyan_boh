use serde_json::json;

use super::{AddRecipeVersion, CreateRecipe, ItemSnapshot};
use crate::quantity::{Quantity, UnitFactor};

fn recipe() -> CreateRecipe {
    serde_json::from_value(json!({
        "command_id": "01890a5d-ac96-774b-bcce-b302099a8500",
        "code": "R-TOAST", "name": "Toast",
        "output_item_id": "01890a5d-ac96-774b-bcce-b302099a8502",
        "active": true,
        "output_input": {"qty": "1.250", "unit_code": "oz", "base_qty_per_unit": "28.349523125"},
        "lines": [
            {"item_id": "01890a5d-ac96-774b-bcce-b302099a8501", "input": {"qty": "25.000", "unit_code": "kg", "base_qty_per_unit": "1000"}},
            {"item_id": "01890a5d-ac96-774b-bcce-b302099a8503", "input": {"qty": "0.500", "unit_code": "mg", "base_qty_per_unit": "0.001"}}
        ]
    })).unwrap()
}

#[test]
fn recipe_version_preserves_inputs_and_converts_each_line_in_wire_order() {
    let command = recipe();
    command.validate().unwrap();
    let snapshot = command.snapshot().unwrap();
    snapshot.validate().unwrap();
    let version = &snapshot.versions[0];
    assert_eq!(version.output_qty_per_batch, Quantity::from_milli(35_437));
    assert_eq!(
        version.lines[0].qty_per_batch,
        Quantity::from_milli(25_000_000)
    );
    assert_eq!(version.lines[1].qty_per_batch, Quantity::from_milli(1));
    assert_eq!(version.output_input, command.output_input);
    for (line, input) in version.lines.iter().zip(&command.lines) {
        assert_eq!(line.input, input.input);
        assert_eq!(line.item_id, input.item_id);
    }
    assert_eq!(
        serde_json::to_string(version).unwrap(),
        r#"{"version":1,"output_qty_per_batch":"35.437","output_input":{"qty":"1.25","unit_code":"oz","base_qty_per_unit":"28.349523125"},"lines":[{"item_id":"01890a5d-ac96-774b-bcce-b302099a8501","qty_per_batch":"25000","input":{"qty":"25","unit_code":"kg","base_qty_per_unit":"1000"}},{"item_id":"01890a5d-ac96-774b-bcce-b302099a8503","qty_per_batch":"0.001","input":{"qty":"0.5","unit_code":"mg","base_qty_per_unit":"0.001"}}]}"#
    );
}

#[test]
fn add_version_retains_unit_choice_even_when_the_converted_quantity_matches() {
    let command = recipe();
    let original = command.snapshot().unwrap().versions.remove(0);
    let mut addition = AddRecipeVersion {
        command_id: command.command_id,
        base_revision: 1,
        output_input: command.output_input,
        lines: command.lines,
    };
    assert_eq!(addition.version(1).unwrap(), original);
    addition.lines[0].input.qty = Quantity::from_milli(2500);
    addition.lines[0].input.unit_code = "bag".into();
    addition.lines[0].input.base_qty_per_unit = UnitFactor::from_nano(10_000_000_000_000);
    let changed = addition.version(2).unwrap();
    assert_eq!(changed.version, 2);
    assert_eq!(
        changed.lines[0].qty_per_batch,
        original.lines[0].qty_per_batch
    );
    assert_ne!(changed.lines[0], original.lines[0]);
    assert!(addition.version(0).is_err());
}

#[test]
fn recipe_commands_and_snapshots_reject_inconsistent_or_duplicate_lines() {
    let command = recipe();
    let mut duplicate = command.clone();
    duplicate.lines.push(duplicate.lines[0].clone());
    assert!(duplicate.validate().is_err());
    let mut too_large = command.clone();
    too_large.output_input.qty = Quantity::from_milli(10_000_000);
    assert!(too_large.validate().is_err());
    let mut empty = command.clone();
    empty.lines.clear();
    assert!(empty.validate().is_err());

    let good = command.snapshot().unwrap();
    for field in ["output", "line", "version", "duplicate"] {
        let mut bad = good.clone();
        let version = &mut bad.versions[0];
        match field {
            "output" => version.output_qty_per_batch = Quantity::from_milli(35_436),
            "line" => version.lines[0].qty_per_batch = Quantity::ZERO,
            "version" => version.version = 2,
            "duplicate" => version.lines.push(version.lines[0].clone()),
            _ => unreachable!(),
        }
        assert!(bad.validate().is_err(), "{field}");
    }
    let mut old_request = serde_json::to_value(command).unwrap();
    old_request["output_qty_per_batch"] = json!("1");
    assert!(serde_json::from_value::<CreateRecipe>(old_request).is_err());
}

#[test]
fn item_unit_factors_accept_nano_precision_and_enforce_both_limits() {
    let mut item: ItemSnapshot = serde_json::from_value(json!({
        "code": "FLOUR", "name": "Flour", "base_unit": "g", "category": "RAW", "active": true,
        "units": [{"unit_code": "tiny", "base_qty_per_unit": "0.000000001"}]
    }))
    .unwrap();
    item.validate().unwrap();
    item.units[0].base_qty_per_unit = UnitFactor::from_nano(1_000_000_000_000_000_000);
    item.validate().unwrap();
    for nano in [-1, 0, 1_000_000_000_000_000_001] {
        item.units[0].base_qty_per_unit = UnitFactor::from_nano(nano);
        assert!(item.validate().is_err());
    }
}
