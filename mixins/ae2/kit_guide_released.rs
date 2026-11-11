fn kit_guide_released_ae2(game: &crate::game::Game<'_>, canceled: &mut bool) {
    ae2_optional::if_kit!("ae2", {
    crate::game::hz_kit_guide_ae2::release(game, canceled);
} else { let _ = (game, canceled); () })
}
