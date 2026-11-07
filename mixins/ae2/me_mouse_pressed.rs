fn ae2_mouse_pressed(game: &crate::game::Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, button: i32, canceled: &mut bool) {
    if game.player.is_none() {
        return;
    }
    if !*canceled {
        crate::game::port_ae2_me::dialog_click(game, screen.mouse, button, canceled);
        if *canceled {
            crate::game::port_ae2_me::press_taken();
        }
    }
    if !*canceled {
        crate::game::port_ae2_me::portable_stack_press(game, screen.rect, screen.mouse, button, canceled);
    }
    if !*canceled {
        crate::game::port_ae2_me::interface_bucket_press(game, screen.mouse, button, canceled);
    }
    if !*canceled {
        crate::game::port_ae2_me::item_screen_click(game, screen.mouse, button, canceled);
    }
    if !*canceled {
        crate::game::port_ae2_me::terminal_middle_click(game, screen.menu, screen.mouse, button, canceled);
    }
    if !*canceled {
        crate::game::port_ae2_me::wireless_click(game, screen.mouse, button, canceled);
    }
    if !*canceled {
        crate::game::port_ae2_me::ghost_press(game, screen, button, canceled);
    }
}
