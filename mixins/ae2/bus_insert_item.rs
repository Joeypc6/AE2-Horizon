fn ae2_bus_insert_item(game: &mut crate::game::Game<'_>, pos: [i32; 3], face: Direction, stack: &ItemStack) -> core::ops::ControlFlow<Option<ItemStack>> {
    match crate::game::port_ae2_me::part_insert(game, pos, face, stack) {
        Some(rest) => core::ops::ControlFlow::Break(rest),
        None => core::ops::ControlFlow::Continue(()),
    }
}
