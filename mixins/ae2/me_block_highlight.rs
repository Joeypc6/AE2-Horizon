fn ae2_block_highlight(game: &crate::game::Game<'_>, pos: [i32; 3], canceled: &mut bool) {
    crate::game::port_ae2_me::block_highlight(game, pos, canceled);
}
