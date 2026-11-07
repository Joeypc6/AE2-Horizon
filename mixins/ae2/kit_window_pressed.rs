fn jei_window_pressed(game: &crate::game::Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, button: i32, canceled: &mut bool) {
    ae2_optional::if_kit_jei!("ae2", {
    if game.player.is_none() || *canceled {
        return;
    }
    crate::game::hz_kit_window_ae2::press(game, screen.mouse, button, canceled);
    if !*canceled {
        crate::game::hz_kit_creative_ae2::press(game, screen.mouse, button, canceled);
    }
} else { () })
}
