fn ae2_tooltip(stack: &horizon_client::items::ItemStack, lines: &mut Vec<(horizon_client::text::Text, u32)>) {
    crate::game::port_ae2_me::tooltip(stack, lines);
}
