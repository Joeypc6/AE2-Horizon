fn ae2_wheel(game: &crate::game::Game<'_>, steps: f32, claimed: &mut bool) {
    crate::game::port_ae2_me::wheel(game, steps, claimed);
}
