fn kit_guide_pressed_ae2(game: &crate::game::Game<'_>, screen: &crate::game::native_hooks::NativeScreenView<'_>, button: i32, canceled: &mut bool) {
    ae2_optional::if_kit!("ae2", {
    if game.player.is_none() {
        return;
    }
    let _ = button;
    crate::game::hz_kit_guide_ae2::click(game, screen.mouse, canceled);
} else { () })
}
