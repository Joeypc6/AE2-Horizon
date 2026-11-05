fn ae2_bus_hopper_eject(game: &mut crate::game::Game<'_>, pos: Pos) -> core::ops::ControlFlow<bool> {
    match crate::game::port_ae2_me::hopper_into_part(game, [pos.0, pos.1, pos.2]) {
        Some(moved) => core::ops::ControlFlow::Break(moved),
        None => core::ops::ControlFlow::Continue(()),
    }
}
