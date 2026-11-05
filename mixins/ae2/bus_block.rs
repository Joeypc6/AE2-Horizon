fn ae2_bus_block(game: &crate::game::Game<'_>, pos: [i32; 3]) -> core::ops::ControlFlow<String> {
    match crate::game::port_ae2_me::part_block(game, pos) {
        Some(block) => core::ops::ControlFlow::Break(block),
        None => core::ops::ControlFlow::Continue(()),
    }
}
