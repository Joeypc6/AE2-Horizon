fn ae2_screen_draw(game: &crate::game::Game<'_>, block: &str, data: &[i32], batch: &mut horizon_client::hud::HudBatch, origin: [f32; 2]) {
    crate::game::port_ae2_me::screen_draw(game, block, data, batch, origin);
    crate::game::port_ae2_me::slot_icons(game, block, batch, origin);
}
