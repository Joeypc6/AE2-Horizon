fn ae2_jei_event(
    game: &mut crate::game::Game<'_>,
    mod_id: &str,
    event: &crate::game::native_hooks::NativePanelEvent,
) {
    ae2_optional::if_mod!("jei", {
    crate::game::port_ae2_jei::event(game, mod_id, event);
} else { () })
}
