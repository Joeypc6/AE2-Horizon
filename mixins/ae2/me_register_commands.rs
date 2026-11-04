fn ae2_register_commands(commands: &mut Vec<crate::game::native_server::NativeCommand>) {
    crate::game::port_ae2_me::register_commands(commands);
}
