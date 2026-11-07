fn kit_guide_render_post_ae2(game: &crate::game::Game<'_>, batch: &mut horizon_client::hud::HudBatch, screen: &crate::game::native_hooks::NativeScreenView<'_>) {
    ae2_optional::if_kit!("ae2", {
    if game.player.is_none() {
        return;
    }
    crate::game::hz_kit_guide_ae2::screen_frame(screen.id, screen.menu);
    crate::game::hz_kit_guide_ae2::draw(game, batch, screen.mouse);
} else { () })
}
