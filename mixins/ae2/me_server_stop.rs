fn ae2_server_stop(game: &mut crate::game::Game<'_>) {
    crate::game::port_ae2_me::tiny_tnt_stop(game);
    crate::game::port_ae2_me::save_cpus(game);
}
