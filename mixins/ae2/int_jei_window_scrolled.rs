fn jei_window_scrolled(game: &crate::game::Game<'_>, scroll: [f64; 2], canceled: &mut bool) {
    ae2_optional::if_mod!("jei", {
    if game.player.is_none() {
        return;
    }
    crate::game::port_jei_window::scroll(scroll[1], canceled);
} else { () })
}
