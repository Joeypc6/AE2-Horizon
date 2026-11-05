fn jei_window_key(game: &crate::game::Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, key: &str, canceled: &mut bool) {
    ae2_optional::if_mod!("jei", {
    if game.player.is_none() {
        return;
    }
    crate::game::port_jei_window::key(game, screen.mouse, key, canceled);
    if !*canceled {
        crate::game::port_jei_creative::key(game, screen.mouse, key, canceled);
    }
} else { () })
}
