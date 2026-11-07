fn ae2_cell_blast(game: &crate::game::Game<'_>, pos: [i32; 3], explosion: Option<f32>, drops: &mut Vec<ItemStack>) {
    crate::game::port_ae2_me::blast_cell_drop(game, pos, explosion, drops);
}
