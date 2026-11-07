fn ae2_key_pressed(game: &crate::game::Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, key: &str, repeat: bool, canceled: &mut bool) {
    if game.player.is_none() {
        return;
    }
    if !*canceled && crate::game::port_ae2_me::jei_key(game, screen.menu, screen.mouse, key) {
        *canceled = true;
    }
    if !*canceled {
        let menu = if screen.id == "mod" { Some("ae2:terminal") } else { screen.menu };
        crate::game::port_ae2_me::screen_key(menu, key, canceled);
    }
}
