fn ae2_screen_tooltip(game: &crate::game::Game<'_>, block: &str, at: [f32; 2], lines: &mut Vec<String>) {
    crate::game::port_ae2_me::screen_tooltip(game, block, at, lines);
}
