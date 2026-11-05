fn jei_window_pressed(game: &crate::game::Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, button: i32, canceled: &mut bool) {
    ae2_optional::if_mod!("jei", {
    if game.player.is_none() {
        return;
    }
    crate::game::port_jei_window::press(game, screen.mouse, button, canceled);
    if !*canceled {
        crate::game::port_jei_window::press_area(game, screen, button, canceled);
    }
    if !*canceled {
        crate::game::port_jei_creative::press(game, screen.mouse, button, canceled);
    }
} else { () })
}
