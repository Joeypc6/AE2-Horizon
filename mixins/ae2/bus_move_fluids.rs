fn ae2_bus_move_fluids(game: &mut crate::game::Game<'_>, dimension: &str, from: [i32; 3], from_face: Direction, to: [i32; 3], to_face: Direction, max: i32) -> core::ops::ControlFlow<Option<(String, i32)>> {
    let from_part = crate::game::port_ae2_me::part_on_face(game, from, from_face);
    let to_part = crate::game::port_ae2_me::part_on_face(game, to, to_face);
    if from_part.is_none() && to_part.is_none() {
        return core::ops::ControlFlow::Continue(());
    }
    core::ops::ControlFlow::Break(game.native_move_fluids(dimension, from_part.unwrap_or(from), from_face, to_part.unwrap_or(to), to_face, max))
}
