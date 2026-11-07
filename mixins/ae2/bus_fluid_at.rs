fn ae2_bus_fluid_at(game: &crate::game::Game<'_>, pos: [i32; 3]) -> core::ops::ControlFlow<Option<FluidAt>> {
    let Some(id) = crate::game::port_ae2_me::part_id_at(game, pos) else {
        return core::ops::ControlFlow::Continue(());
    };
    let mut tanks = None;
    crate::game::port_ae2_me::block_entity_fluids(&id, &mut tanks);
    let there = matches!(game.world.block_entities.get(&(pos[0], pos[1], pos[2])), Some(horizon_client::containers::BlockEntity::Mod(m)) if !m.id.is_empty());
    core::ops::ControlFlow::Break(tanks.filter(|_| there).map(|t| FluidAt::Mod { tanks: Arc::new(t), block: id.clone() }))
}
