fn ae2_bus_fluid_extract(game: &mut crate::game::Game<'_>, dimension: &str, pos: [i32; 3], face: Direction, amount: i32) -> core::ops::ControlFlow<Option<(String, i32)>> {
    let Some(part) = crate::game::port_ae2_me::part_on_face(game, pos, face) else {
        return core::ops::ControlFlow::Continue(());
    };
    core::ops::ControlFlow::Break(game.native_fluid_extract(dimension, part, face, amount))
}
