fn ae2_be_energy(block: &str, energy: &mut Option<horizon_client::containers::ModEnergyBuffer>) {
    crate::game::port_ae2_me::block_entity_energy(block, energy);
}
