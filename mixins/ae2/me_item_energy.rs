fn ae2_item_energy(item: &str, buffer: &mut Option<crate::game::native_item_energy::ItemEnergyBuffer>) {
    crate::game::port_ae2_me::item_energy_buffer(item, buffer);
}
