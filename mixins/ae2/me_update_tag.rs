fn ae2_update_tag(entity: &horizon_client::containers::ModBlockEntity, tag: &mut Option<horizon_world::nbt::Compound>) {
    crate::game::port_ae2_me::update_tag(entity, tag);
}
