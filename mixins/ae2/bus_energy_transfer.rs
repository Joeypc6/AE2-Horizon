fn ae2_bus_energy_transfer(game: &mut crate::game::Game<'_>, dimension: &str, pos: [i32; 3], face: Direction, amount: i32, insert: bool, simulate: bool) -> core::ops::ControlFlow<i32> {
    let Some(part) = crate::game::port_ae2_me::part_on_face(game, pos, face) else {
        return core::ops::ControlFlow::Continue(());
    };
    core::ops::ControlFlow::Break(game.native_energy_transfer(dimension, part, face, amount, insert, simulate))
}
