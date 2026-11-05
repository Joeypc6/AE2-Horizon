fn ae2_bus_set_block(game: &mut crate::game::Game<'_>, pos: [i32; 3], state: BlockStateId) -> core::ops::ControlFlow<bool> {
    match crate::game::port_ae2_me::part_set_block(game, pos, state) {
        Some(set) => core::ops::ControlFlow::Break(set),
        None => core::ops::ControlFlow::Continue(()),
    }
}
