fn jei_window_scrolled(game: &crate::game::Game<'_>, scroll: [f64; 2], canceled: &mut bool) {
    ae2_optional::if_kit_jei!("ae2", {
    if game.player.is_none() || *canceled {
        return;
    }
    crate::game::hz_kit_window_ae2::scroll(scroll[1], canceled);
} else { let _ = (game, scroll, canceled); () })
}
