fn ae2_kit_frame(game: &mut crate::game::Game<'_>) {
    if !game.native_on("ae2") {
        return;
    }
    crate::game::port_ae2_me::kit_guide::register("ae2", "AE2 Guide", "AE2 guide");
    crate::game::port_ae2_me::kit_guide::register_keyboard_taken(crate::game::port_ae2_me::keyboard_taken);
    crate::game::port_ae2_me::kit_guide::set_hotkey_enabled(crate::game::port_ae2_me::guide_hotkey_enabled());
}
