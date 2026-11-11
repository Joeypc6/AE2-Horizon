// `frame`: the layer's work while JEI is on; its window shut when JEI is turned off.
fn kit_layer_frame_ae2(game: &mut crate::game::Game<'_>) {
    ae2_optional::if_kit_jei!("ae2", {
    if game.native_on(crate::game::jei::ID) {
        crate::game::hz_kit_layer_ae2::frame(game);
    } else if crate::game::hz_kit_window_ae2::is_open() {
        // JEI turned off with its window open: nothing would close it, and it would keep taking the screen's input.
        crate::game::hz_kit_window_ae2::close();
        let _ = crate::game::hz_kit_window_ae2::take_acts();
    }
} else { let _ = game; () })
}
