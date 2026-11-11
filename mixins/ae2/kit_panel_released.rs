fn jei_window_released(screen: &crate::game::native_hooks::NativeScreenView<'_>) {
    ae2_optional::if_kit_jei!("ae2", {
    crate::game::hz_kit_window_ae2::release(screen.mouse);
} else { let _ = screen; () })
}
