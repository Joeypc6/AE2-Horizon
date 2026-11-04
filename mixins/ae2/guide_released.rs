fn ae2_guide_released(game: &crate::game::Game<'_>, canceled: &mut bool) {
    crate::game::port_ae2_guide::release(game, canceled);
    if !*canceled {
        crate::game::port_ae2_me::ghost_release(game, canceled);
    }
}
