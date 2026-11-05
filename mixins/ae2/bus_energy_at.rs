fn ae2_bus_energy_at(game: &crate::game::Game<'_>, pos: [i32; 3]) -> core::ops::ControlFlow<Option<(Arc<ModEnergyBuffer>, i32)>> {
    let Some(id) = crate::game::port_ae2_me::part_id_at(game, pos) else {
        return core::ops::ControlFlow::Continue(());
    };
    let mut buffer = None;
    crate::game::port_ae2_me::block_entity_energy(&id, &mut buffer);
    let at = game.world.block_entities.get(&(pos[0], pos[1], pos[2]));
    core::ops::ControlFlow::Break(match (buffer, at) {
        (Some(b), Some(horizon_client::containers::BlockEntity::Mod(m))) if !m.id.is_empty() => Some((Arc::new(b), read_energy(&m.nbt))),
        _ => None,
    })
}
