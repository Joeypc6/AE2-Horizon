fn jei_window_char(game: &crate::game::Game<'_>, c: char, canceled: &mut bool) {
    ae2_optional::if_mod!("jei", {
    if game.player.is_none() {
        return;
    }
    if crate::game::port_jei_window::is_open() && !c.is_control() {
        // Typed into the panel's search box when it has the focus; the game's own hook hands it over.
    }
    crate::game::port_jei_creative::char_typed(c, canceled);
} else { () })
}
