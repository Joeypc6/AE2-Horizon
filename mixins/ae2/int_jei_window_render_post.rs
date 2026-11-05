fn jei_window_render_post(game: &crate::game::Game<'_>, batch: &mut horizon_client::hud::HudBatch, screen: &crate::game::native_hooks::NativeScreenView<'_>) {
    ae2_optional::if_mod!("jei", {
    if game.player.is_none() {
        return;
    }
    crate::game::port_jei_window::area_tooltip(game, batch, screen);
} else { () })
}
