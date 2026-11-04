fn ae2_analog_output(level: &crate::game::GameLevel<'_>, pos: horizon_core::position::BlockPos, state: horizon_world::registry::BlockStateId, output: &mut i32) {
    let name: &str = &level.facts.get(state).name;
    if !name.starts_with("ae2:") {
        return;
    }
    if let Some(signal) = crate::game::port_ae2_me::energy_cell_signal(name, [pos.x, pos.y, pos.z]) {
        *output = signal;
        return;
    }
    let Some((slots, size)) = crate::game::port_ae2_me::comparator_slots(name) else { return };
    // InternalInventory.getRedstoneSignal (AbstractContainerMenu.getRedstoneSignalFromContainer).
    let mut total = 0.0f32;
    let mut any = false;
    if let Some(horizon_client::containers::BlockEntity::Mod(m)) = level.world.block_entities.get(&(pos.x, pos.y, pos.z)) {
        for stack in slots.iter().filter_map(|i| m.items.get(*i)).flatten() {
            let most = level.items.get(&stack.item).map_or(64, |i| i.max_stack.clamp(1, 64));
            total += stack.count as f32 / most as f32;
            any = true;
        }
    }
    total /= size as f32;
    *output = (total.min(1.0) * 14.0).floor() as i32 + i32::from(any);
}
