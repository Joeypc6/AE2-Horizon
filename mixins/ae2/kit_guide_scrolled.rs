fn kit_guide_scrolled_ae2(game: &crate::game::Game<'_>, scroll: [f64; 2], canceled: &mut bool) {
    ae2_optional::if_kit!("ae2", {
    if game.player.is_some() {
        crate::game::hz_kit_guide_ae2::scroll(scroll[1], canceled);
    }
} else { () })
}
