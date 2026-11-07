fn ae2_guide_released(game: &crate::game::Game<'_>, canceled: &mut bool) {
    crate::game::port_ae2_me::scrollbar_release(canceled);
    if *canceled {
        return;
    }
    crate::game::port_ae2_me::ghost_release(game, canceled);
}
