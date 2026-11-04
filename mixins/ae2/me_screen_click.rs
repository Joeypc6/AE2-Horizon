fn ae2_screen_click(game: &crate::game::Game<'_>, block: &str, at: [f32; 2], right: bool, button: &mut Option<i32>) {
    crate::game::port_ae2_me::screen_click(game, block, at, right, button);
}
