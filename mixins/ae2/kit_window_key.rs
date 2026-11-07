fn jei_window_key(game: &crate::game::Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, key: &str, canceled: &mut bool) {
    ae2_optional::if_kit_jei!("ae2", {
    if game.player.is_none() || *canceled {
        return;
    }
    crate::game::hz_kit_window_ae2::key(game, screen.mouse, key, canceled);
    if !*canceled {
        crate::game::hz_kit_creative_ae2::key(game, screen.mouse, key, canceled);
    }
} else { () })
}
