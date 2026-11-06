fn ae2_before_menu_click(game: &mut crate::game::Game<'_>, host: &LanHost, conn: u32) {
    if let Some(Slots::Block(pos)) = host.hub.guest(conn).and_then(|g| g.menu.as_ref()).map(|m| m.slots) {
        crate::game::port_ae2_me::before_menu_click(game, [pos.0, pos.1, pos.2]);
    }
}
