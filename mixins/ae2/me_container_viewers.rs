fn ae2_container_viewers(game: &mut crate::game::Game<'_>, pos: [i32; 3], viewers: u32, previous: u32) {
    crate::game::port_ae2_me::container_viewers(game, pos, viewers, previous);
}
