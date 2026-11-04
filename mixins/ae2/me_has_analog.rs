fn ae2_has_analog(level: &crate::game::GameLevel<'_>, state: horizon_world::registry::BlockStateId, output: &mut bool) {
    let name: &str = &level.facts.get(state).name;
    if name.starts_with("ae2:") && (crate::game::port_ae2_me::comparator_slots(name).is_some() || matches!(name, "ae2:energy_cell" | "ae2:dense_energy_cell")) {
        *output = true;
    }
}
