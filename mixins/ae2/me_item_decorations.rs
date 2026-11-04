fn ae2_item_decorations(game: &crate::game::Game<'_>, stack: &horizon_client::items::ItemStack, x: f32, y: f32, batch: &mut horizon_client::hud::HudBatch) {
    crate::game::port_ae2_me::pattern_decoration(game, stack, x, y, batch);
}
