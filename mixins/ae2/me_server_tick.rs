fn ae2_server_tick(game: &mut crate::game::Game<'_>, phase: &crate::game::native_lifecycle::ServerTickPhase) {
    if matches!(phase, crate::game::native_lifecycle::ServerTickPhase::ServerTickEnd { .. }) {
        crate::game::port_ae2_me::server_tick(game);
    }
}
