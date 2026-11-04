fn ae2_be(block: &str, entity: &mut Option<(String, horizon_world::nbt::Compound)>) {
    crate::game::port_ae2_me::block_entity(block, entity);
}
