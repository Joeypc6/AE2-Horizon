fn ae2_bus_fluid_insert(game: &mut crate::game::Game<'_>, pos: [i32; 3], face: Direction, fluid: &str, amount: i32) -> core::ops::ControlFlow<i32> {
    let Some(part) = crate::game::port_ae2_me::part_on_face(game, pos, face) else {
        return core::ops::ControlFlow::Continue(());
    };
    core::ops::ControlFlow::Break(game.native_fluid_insert(part, face, fluid, amount))
}
