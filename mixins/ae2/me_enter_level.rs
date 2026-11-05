// AE2's tables follow the level in place, on a server.
fn ae2_enter_level(game: &crate::game::Game<'_>, target: Dim) {
    if !game.options.execution_role.is_client() {
        crate::game::port_ae2_me::level_in_place(game.dim, target);
    }
}
