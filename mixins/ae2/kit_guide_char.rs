fn kit_guide_char_ae2(game: &crate::game::Game<'_>, c: char, canceled: &mut bool) {
    ae2_optional::if_kit!("ae2", {
    let _ = c;
    if game.player.is_some() && crate::game::hz_kit_guide_ae2::is_open() {
        *canceled = true;
    }
} else { let _ = (game, c, canceled); () })
}
