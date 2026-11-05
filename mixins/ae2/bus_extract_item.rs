fn ae2_bus_extract_item(game: &mut crate::game::Game<'_>, dimension: &str, pos: [i32; 3], face: Direction, slot: usize, count: u32) -> core::ops::ControlFlow<Option<ItemStack>> {
    let Some(part) = crate::game::port_ae2_me::part_on_face(game, pos, face) else {
        return core::ops::ControlFlow::Continue(());
    };
    core::ops::ControlFlow::Break(game.native_extract_item(dimension, part, face, slot, count))
}
