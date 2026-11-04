fn ae2_button_clicker(host: &LanHost, conn: u32) {
    if let Some(g) = host.hub.guest(conn) {
        crate::game::port_ae2_me::button_clicker(g.uuid, g.game_mode);
    }
}
