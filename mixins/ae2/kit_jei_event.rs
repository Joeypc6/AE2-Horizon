fn ae2_jei_event(
    game: &mut crate::game::Game<'_>,
    mod_id: &str,
    event: &crate::game::native_hooks::NativePanelEvent,
) {
    ae2_optional::if_kit_jei!("ae2", {
    crate::game::hz_kit_layer_ae2::event(game, mod_id, event);
} else { let _ = (game, mod_id, event); () })
}
