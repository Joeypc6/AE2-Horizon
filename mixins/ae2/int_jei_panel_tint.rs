fn jei_panel_tint(game: &Game<'_>, batch: &mut HudBatch) {
    ae2_optional::if_mod!("jei", {
    crate::game::port_jei_window::tint(game, batch);
} else { () })
}
