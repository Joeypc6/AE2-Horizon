fn ae2_screen_scrolled(screen: &crate::game::native_hooks::NativeScreenView<'_>, scroll: [f64; 2], canceled: &mut bool) {
    let menu = if screen.id == "mod" { Some("ae2:terminal") } else { screen.menu };
    crate::game::port_ae2_me::scrolled(menu, scroll, canceled);
}
