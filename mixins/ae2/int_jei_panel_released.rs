fn jei_window_released(screen: &crate::game::native_hooks::NativeScreenView<'_>) {
    ae2_optional::if_mod!("jei", {
    crate::game::port_jei_window::release(screen.mouse);
} else { () })
}
