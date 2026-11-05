fn jei_hud_after(game: &mut crate::game::Game<'_>, batch: &mut horizon_client::hud::HudBatch) {
    ae2_optional::if_mod!("jei", {
    crate::game::port_jei_window::hud_after(game, batch);
} else { () })
}
