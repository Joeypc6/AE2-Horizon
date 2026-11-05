fn ae2_bus_update_signal(game: &mut crate::game::Game<'_>, dimension: &str, pos: [i32; 3]) -> core::ops::ControlFlow<bool> {
    if pos[1] < 1024 {
        return core::ops::ControlFlow::Continue(());
    }
    core::ops::ControlFlow::Break(game.native_update_signal_neighbors(dimension, crate::game::port_ae2_me::host_of(pos)))
}
