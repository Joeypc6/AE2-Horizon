fn ae2_area_tooltip(game: &crate::game::Game<'_>, batch: &mut horizon_client::hud::HudBatch, screen: &crate::game::native_hooks::NativeScreenView<'_>) {
    ae2_optional::if_mod!("jei", {
    if game.player.is_some() {
        crate::game::port_ae2_jei::area_tooltip(game, batch, screen);
    }
} else { () })
}
