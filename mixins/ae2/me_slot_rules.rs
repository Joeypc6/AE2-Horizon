fn ae2_menu_slot_rules(menu: &str, slot: usize, stack: &ItemStack, may_place: &mut bool, max_stack: &mut u32) {
    // RestrictedInputSlot's PlacableItemType and OutputSlot, for each of AE2's menus.
    let Some(menu) = menu.strip_prefix("ae2:") else {
        return;
    };
    let name = stack.item.strip_prefix("ae2:").unwrap_or("");
    let cell = name.starts_with("item_storage_cell_")
        || name.starts_with("fluid_storage_cell_")
        || name == "creative_item_cell"
        || name == "creative_fluid_cell"
        || name == "color_applicator"
        || name == "matter_cannon";
    let molecular = name == "crafting_pattern" || name == "smithing_table_pattern" || name == "stonecutting_pattern";
    let pattern = molecular || name == "processing_pattern";
    let card = name == "capacity_card"
        || name == "crafting_card"
        || name == "energy_card"
        || name == "equal_distribution_card"
        || name == "fuzzy_card"
        || name == "inverter_card"
        || name == "redstone_card"
        || name == "speed_card"
        || name == "void_card";
    // InscriberRecipes.isValidOptionalIngredient, and the name press.
    let plate = name.ends_with("_press") || name.starts_with("printed_");
    let (place, most) = match (menu, slot) {
        ("drive" | "chest", _) => (cell, 1),
        ("io_port", 0..=5) => (cell, 1),
        ("io_port", 6..=11) => (false, 1),
        ("io_port", _) => (card, 1),
        ("wireless_access_point", 0) => (name == "wireless_booster", 64),
        ("wireless_access_point", 1) => (name == "wireless_terminal" || name == "wireless_crafting_terminal", 1),
        ("wireless_access_point", _) => (false, 1),
        ("spatial_io_port", 0) => (name.starts_with("spatial_storage_cell_"), 1),
        ("spatial_io_port", _) => (false, 1),
        ("quantum_link", _) => (name == "quantum_entangled_singularity", 1),
        ("condenser", 2) => (name.starts_with("cell_component_"), 1),
        ("pattern_provider", 0..=8) => (pattern, 1),
        ("pattern_provider", _) => (false, 1),
        ("molecular_assembler", 0) => (molecular, 1),
        ("molecular_assembler", 1..=5) => (card, 1),
        ("molecular_assembler", _) => (false, 1),
        ("inscriber", 0 | 2) => (plate, 64),
        ("inscriber", 4..) => (card, 1),
        ("vibration_chamber", 1..) => (card, 1),
        ("cell_workbench", 0) => (cell || name == "view_cell" || name.starts_with("portable_"), 1),
        ("cell_workbench", _) => (card, 1),
        ("interface", 9) | ("level_emitter", 1) => (card, 1),
        ("io_bus" | "storage_bus", _) => (card, 1),
        ("terminal", 1..=5) | ("crafting_terminal", 11..=15) | ("pattern_encoding_terminal", 2..=6) => (name == "view_cell", 1),
        ("pattern_encoding_terminal", 0) => (name == "blank_pattern", 64),
        ("pattern_encoding_terminal", 1) => (pattern, 1),
        _ => return,
    };
    *may_place = place;
    *max_stack = most;
}
