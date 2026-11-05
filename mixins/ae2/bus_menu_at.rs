fn ae2_bus_menu_at(game: &crate::game::Game<'_>, key: Pos) -> core::ops::ControlFlow<Option<&'static str>> {
    let Some(block) = crate::game::port_ae2_me::part_block(game, [key.0, key.1, key.2]) else {
        return core::ops::ControlFlow::Continue(());
    };
    let mut container = None;
    game.native_block_entity_container(&block, &mut container);
    core::ops::ControlFlow::Break(container.and_then(|c| mod_menu(c.menu.as_deref()?).map(|(menu, _)| menu)))
}
