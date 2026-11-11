fn jei_panel_tint(game: &Game<'_>, batch: &mut HudBatch) {
    ae2_optional::if_kit_jei!("ae2", {
    crate::game::hz_kit_window_ae2::tint(game, batch);
} else { let _ = (game, batch); () })
}
