fn ae2_right_click(game: &mut crate::game::Game<'_>, pos: [i32; 3], face: [i32; 3], canceled: &mut bool) {
    crate::game::port_ae2_me::right_click(game, pos, face, canceled);
}
