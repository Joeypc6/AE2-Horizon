fn ae2_destroy_block(game: &mut crate::game::Game<'_>, pos: [i32; 3], canceled: &mut bool) {
    crate::game::port_ae2_me::destroyed(game, pos, canceled);
}
