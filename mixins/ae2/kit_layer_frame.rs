fn kit_layer_frame_ae2(game: &mut crate::game::Game<'_>) {
    ae2_optional::if_kit_jei!("ae2", {
    if game.native_on(crate::game::jei::ID) {
        crate::game::hz_kit_layer_ae2::frame(game);
    }
} else { () })
}
