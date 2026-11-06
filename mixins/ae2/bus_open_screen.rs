fn ae2_bus_open_screen(game: &mut crate::game::Game<'_>, key: Pos, target: Option<Pos>, window: i32, menu: &str) -> core::ops::ControlFlow<()> {
    let Some(t) = target else {
        return core::ops::ControlFlow::Continue(());
    };
    let ids = crate::game::port_ae2_me::part_ids_for_screen(game, [t.0, t.1, t.2]);
    for block in ids {
        let mut container = None;
        game.native_block_entity_container(&block, &mut container);
        if container.and_then(|c| mod_menu(c.menu.as_deref()?).map(|(m, _)| m)) != Some(menu) {
            continue;
        }
        let mut screen = None;
        game.native_screen(&block, &mut screen);
        if let Some(screen) = screen {
            game.mod_screen = Some(OpenNativeScreen { key, window, block, screen, data: Vec::new(), extra: None });
            return core::ops::ControlFlow::Break(());
        }
    }
    core::ops::ControlFlow::Continue(())
}
