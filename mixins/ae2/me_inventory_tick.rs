fn ae2_inventory_tick(game: &mut crate::game::Game<'_>, slot: i32) {
    crate::game::port_ae2_me::inventory_tick(game, slot);
}
