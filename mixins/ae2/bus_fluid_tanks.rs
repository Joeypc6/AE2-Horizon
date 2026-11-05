fn ae2_bus_fluid_tanks(game: &crate::game::Game<'_>, dimension: &str, pos: [i32; 3], face: Direction) -> core::ops::ControlFlow<Option<Vec<NativeFluidTank>>> {
    let Some(part) = crate::game::port_ae2_me::part_on_face(game, pos, face) else {
        return core::ops::ControlFlow::Continue(());
    };
    core::ops::ControlFlow::Break(game.native_fluid_tanks(dimension, part, face))
}
