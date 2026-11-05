fn ae2_bus_view(world_of: &Containers<'_>, pos: Pos) -> core::ops::ControlFlow<Option<View>> {
    if pos.1 < 1024 {
        return core::ops::ControlFlow::Continue(());
    }
    let Some(BlockEntity::Mod(m)) = world_of.world.block_entities.get(&pos) else {
        return core::ops::ControlFlow::Break(None);
    };
    let mut container = None;
    crate::game::port_ae2_me::block_entity_container(&m.id, &mut container);
    core::ops::ControlFlow::Break(container.filter(|_| !m.id.is_empty()).map(|c| View { parts: vec![(pos, c.slots)], kind: Kind::Mod, container: Some(Arc::new(c)) }))
}
