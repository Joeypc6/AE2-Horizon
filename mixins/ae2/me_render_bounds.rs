fn ae2_render_bounds(entity: &horizon_client::containers::ModBlockEntity, bounds: &mut [f32; 6]) {
    crate::game::port_ae2_me::render_bounds(entity, bounds);
}
