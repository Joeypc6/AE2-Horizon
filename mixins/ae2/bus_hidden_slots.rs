fn ae2_bus_hidden_slots(game: &crate::game::Game<'_>, pos: Pos, size: usize) -> core::ops::ControlFlow<Vec<usize>> {
    let Some(block) = crate::game::port_ae2_me::part_block(game, [pos.0, pos.1, pos.2]) else {
        return core::ops::ControlFlow::Continue(());
    };
    let mut screen = None;
    game.native_screen(&block, &mut screen);
    let Some(screen) = screen else {
        return core::ops::ControlFlow::Break(Vec::new());
    };
    core::ops::ControlFlow::Break((0..size).filter(|&i| screen.slots.get(i).is_none_or(Option::is_none)).collect())
}
