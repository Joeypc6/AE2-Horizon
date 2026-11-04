fn ae2_use_item(game: &mut crate::game::Game<'_>, stack: &horizon_client::items::ItemStack, claimed: &mut bool) {
    crate::game::port_ae2_me::use_item(game, stack, claimed);
}
