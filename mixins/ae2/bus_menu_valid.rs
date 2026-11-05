fn ae2_bus_menu_valid(slots: Slots, world: &World, guest: &Guest) -> core::ops::ControlFlow<bool> {
    let Slots::Block(k) = slots else {
        return core::ops::ControlFlow::Continue(());
    };
    if k.1 < 1024 {
        return core::ops::ControlFlow::Continue(());
    }
    // Its reach is its cell's (me.rs `host_of`).
    let y = (k.1 - 1024).div_euclid(6) - 64;
    core::ops::ControlFlow::Break(world.block_entities.contains_key(&k) && guest.within_block_range([k.0, y, k.2], 4.0))
}
