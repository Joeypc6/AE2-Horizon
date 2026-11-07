fn ae2_char_typed(game: &crate::game::Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, c: char, canceled: &mut bool) {
    if game.player.is_none() {
        return;
    }
    if *canceled {
        return;
    }
    if crate::game::port_ae2_me::jei_char(c) {
        *canceled = true;
        return;
    }
    let menu = if screen.id == "mod" { Some("ae2:terminal") } else { screen.menu };
    crate::game::port_ae2_me::screen_char(menu, c, canceled);
}
