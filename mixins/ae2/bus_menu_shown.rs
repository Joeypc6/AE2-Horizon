fn ae2_bus_menu_shown(world: &World, k: (i32, i32, i32)) -> core::ops::ControlFlow<Option<&[Option<ItemStack>]>> {
    if k.1 < 1024 {
        return core::ops::ControlFlow::Continue(());
    }
    core::ops::ControlFlow::Break(world.block_entities.get(&k).map(|e| e.slots()))
}
