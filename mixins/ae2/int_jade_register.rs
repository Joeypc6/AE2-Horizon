fn jade_ae2_register(game: &mut crate::game::Game<'_>) {
    ae2_optional::if_mod!("jade", {
    if game.native_on("ae2") {
        crate::game::jade_ae2::register();
    }
} else { () })
}
