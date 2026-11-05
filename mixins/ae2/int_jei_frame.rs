fn ae2_jei_frame(game: &mut crate::game::Game<'_>) {
    ae2_optional::if_mod!("jei", {
    if !game.native_on("ae2") {
        return;
    }
    crate::game::port_ae2_jei::frame(game);
} else { () })
}
