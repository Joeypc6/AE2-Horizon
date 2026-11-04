fn ae2_left_click(game: &mut crate::game::Game<'_>, pos: [i32; 3], canceled: &mut bool) {
    if crate::game::port_ae2_me::monitor_punch(game, pos) {
        *canceled = true;
    }
}
