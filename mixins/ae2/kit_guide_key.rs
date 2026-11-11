fn kit_guide_key_ae2(game: &crate::game::Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, key: &str, repeat: bool, canceled: &mut bool) {
    ae2_optional::if_kit!("ae2", {
    if game.player.is_none() {
        return;
    }
    crate::game::hz_kit_guide_ae2::key(game, screen.menu, key, repeat, canceled);
} else { let _ = (game, screen, key, repeat, canceled); () })
}
