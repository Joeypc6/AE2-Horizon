fn ae2_perform_console(game: &mut crate::game::Game<'_>, command: &str, ran: &mut bool) {
    crate::game::port_ae2_me::perform_console_command(game, command, ran);
}
