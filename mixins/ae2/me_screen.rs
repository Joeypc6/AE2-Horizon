fn ae2_screen(game: &crate::game::Game<'_>, block: &str, screen: &mut Option<crate::game::native_hooks::NativeScreen>) {
    crate::game::port_ae2_me::screen(game, block, screen);
}
