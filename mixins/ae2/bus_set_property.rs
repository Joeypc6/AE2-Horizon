fn ae2_bus_set_property(game: &crate::game::Game<'_>, pos: [i32; 3], key: &str, value: &str) -> core::ops::ControlFlow<bool> {
    match crate::game::port_ae2_me::part_set_property(game, pos, key, value) {
        Some(set) => core::ops::ControlFlow::Break(set),
        None => core::ops::ControlFlow::Continue(()),
    }
}
