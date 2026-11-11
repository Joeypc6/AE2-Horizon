fn jei_hud_before(game: &mut crate::game::Game<'_>) {
    ae2_optional::if_kit_jei!("ae2", {
    crate::game::hz_kit_window_ae2::hud_before(game);
} else { let _ = game; () })
}
