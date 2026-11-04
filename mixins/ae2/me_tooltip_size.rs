fn ae2_tooltip_size(game: &crate::game::Game<'_>, stack: &horizon_client::items::ItemStack, size: &mut Option<[f32; 2]>) {
    crate::game::port_ae2_me::tooltip_component(game, stack, size);
}
