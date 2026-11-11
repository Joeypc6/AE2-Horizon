fn jei_hud_after(game: &mut crate::game::Game<'_>, batch: &mut horizon_client::hud::HudBatch) {
    ae2_optional::if_kit_jei!("ae2", {
    crate::game::hz_kit_window_ae2::hud_after(game, batch);
} else { let _ = (game, batch); () })
}
