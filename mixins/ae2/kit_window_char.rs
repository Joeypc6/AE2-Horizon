fn jei_window_char(game: &crate::game::Game<'_>, c: char, canceled: &mut bool) {
    ae2_optional::if_kit_jei!("ae2", {
    if game.player.is_none() || *canceled {
        return;
    }
    // The panel's search box takes it first, when it has the focus.
    crate::game::hz_kit_creative_ae2::char_typed(c, canceled);
    // Over the window the keys are the window's: their characters never reach the screen behind it (the creative
    // search would take a recipe key's letter).
    if !*canceled && !c.is_control() && crate::game::hz_kit_window_ae2::takes_keys(game) {
        *canceled = true;
    }
} else { let _ = (game, c, canceled); () })
}
