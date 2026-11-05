fn jei_hud_before(game: &mut crate::game::Game<'_>) {
    ae2_optional::if_mod!("jei", {
    crate::game::port_jei_window::hud_before(game);
} else { () })
}
