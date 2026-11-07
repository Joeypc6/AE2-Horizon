fn jei_window_char(game: &crate::game::Game<'_>, c: char, canceled: &mut bool) {
    ae2_optional::if_kit_jei!("ae2", {
    if game.player.is_none() || *canceled {
        return;
    }
    if crate::game::hz_kit_window_ae2::is_open() && !c.is_control() {
        // Typed into the panel's search box when it has the focus; the game's own hook hands it over.
    }
    crate::game::hz_kit_creative_ae2::char_typed(c, canceled);
} else { () })
}
