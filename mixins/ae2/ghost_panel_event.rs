fn ae2_ghost_panel_event(mod_id: &str, event: &crate::game::native_hooks::NativePanelEvent) {
    let _ = mod_id;
    crate::game::port_ae2_me::ghost_panel_event(event);
}
