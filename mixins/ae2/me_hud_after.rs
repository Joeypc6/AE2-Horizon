fn ae2_hud_after(game: &mut crate::game::Game<'_>, mut batch: HudBatch) -> HudBatch {
    crate::game::port_ae2_me::wireless_jei_draw(game, &mut batch);
    crate::game::port_ae2_me::job_toast_draw(game, &mut batch);
    batch
}
