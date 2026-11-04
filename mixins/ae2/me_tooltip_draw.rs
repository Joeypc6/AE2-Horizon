fn ae2_tooltip_draw(game: &crate::game::Game<'_>, stack: &horizon_client::items::ItemStack, batch: &mut horizon_client::hud::HudBatch, origin: [f32; 2]) {
    crate::game::port_ae2_me::tooltip_component_draw(game, stack, batch, origin);
}
