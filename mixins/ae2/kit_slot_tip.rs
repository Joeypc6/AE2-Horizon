fn jei_slot_tip(game: &crate::game::Game<'_>) -> core::ops::ControlFlow<()> {
    ae2_optional::if_kit_jei!("ae2", {
    if crate::game::hz_kit_window_ae2::defer_tooltip(game) {
        return core::ops::ControlFlow::Break(());
    }
} else { let _ = game; () });
    core::ops::ControlFlow::Continue(())
}
