fn kit_guide_frame_ae2(game: &mut crate::game::Game<'_>) {
    ae2_optional::if_kit!("ae2", {
    if game.player.is_some() {
        crate::game::hz_kit_guide_ae2::frame_hook(game);
    }
} else { () })
}
