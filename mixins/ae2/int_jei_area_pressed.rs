fn ae2_area_pressed(game: &crate::game::Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, button: i32, canceled: &mut bool) {
    ae2_optional::if_mod!("jei", {
    if game.player.is_some() && !*canceled {
        crate::game::port_ae2_jei::press_area(game, screen, button, canceled);
    }
} else { () })
}
