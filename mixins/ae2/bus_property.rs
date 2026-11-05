fn ae2_bus_property(game: &crate::game::Game<'_>, pos: [i32; 3], key: &str) -> core::ops::ControlFlow<Option<String>> {
    match crate::game::port_ae2_me::part_property(game, pos, key) {
        Some(value) => core::ops::ControlFlow::Break(value),
        None => core::ops::ControlFlow::Continue(()),
    }
}
