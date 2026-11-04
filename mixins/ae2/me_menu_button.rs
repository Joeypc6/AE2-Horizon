fn ae2_menu_button(game: &mut crate::game::Game<'_>, pos: [i32; 3], entity: &mut horizon_client::containers::ModBlockEntity, id: i32, carried: &mut Option<horizon_client::items::ItemStack>) {
    crate::game::port_ae2_me::menu_button(game, pos, entity, id, carried);
    crate::game::port_ae2_me::button_done();
}
