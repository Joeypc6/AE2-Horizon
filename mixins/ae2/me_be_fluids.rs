fn ae2_be_fluids(block: &str, fluids: &mut Option<horizon_client::containers::ModFluidTanks>) {
    crate::game::port_ae2_me::block_entity_fluids(block, fluids);
}
