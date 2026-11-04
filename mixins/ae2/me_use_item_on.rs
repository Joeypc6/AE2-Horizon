fn ae2_use_item_on(game: &mut crate::game::Game<'_>, stack: &horizon_client::items::ItemStack, pos: [i32; 3], face: [i32; 3], claimed: &mut bool) {
    crate::game::port_ae2_me::clear_paint_for(game, stack, pos, face);
    crate::game::port_ae2_me::use_item_on(game, stack, pos, face, claimed);
}
