fn ae2_bus_sync(pos: [i32; 3]) -> core::ops::ControlFlow<()> {
    if crate::game::port_ae2_me::part_sync(pos) {
        return core::ops::ControlFlow::Break(());
    }
    core::ops::ControlFlow::Continue(())
}
