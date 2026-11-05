fn ae2_bus_container_slots(game: &crate::game::Game<'_>, dimension: &str, pos: [i32; 3], face: Direction) -> core::ops::ControlFlow<Option<Vec<NativeContainerSlot>>> {
    let Some(part) = crate::game::port_ae2_me::part_on_face(game, pos, face) else {
        return core::ops::ControlFlow::Continue(());
    };
    core::ops::ControlFlow::Break(game.native_container_slots(dimension, part, face))
}
