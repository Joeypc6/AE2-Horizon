fn ae2_render_post(game: &crate::game::Game<'_>, batch: &mut horizon_client::hud::HudBatch, screen: &crate::game::native_hooks::NativeScreenView<'_>) {
    if game.player.is_none() {
        return;
    }
    crate::game::port_ae2_me::wireless_draw(game, batch, screen.mouse);
    crate::game::port_ae2_me::item_screen_draw(game, batch, screen.mouse);
    crate::game::port_ae2_me::dialog_draw(game, batch, screen.mouse);
    crate::game::port_ae2_me::ghost_draw(game, batch, screen.mouse);
}
