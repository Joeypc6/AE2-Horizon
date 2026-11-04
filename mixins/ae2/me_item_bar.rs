fn ae2_item_bar(stack: &horizon_client::items::ItemStack, bar: &mut Option<(i32, u32)>) {
    crate::game::port_ae2_me::item_bar(stack, bar);
}
