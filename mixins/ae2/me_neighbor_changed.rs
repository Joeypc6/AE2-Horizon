fn ae2_neighbor_changed(level: &GameLevel<'_>, pos: BlockPos, state: BlockStateId) {
    crate::game::port_ae2_me::neighbor_changed(&level.facts.get(state).name, [pos.x, pos.y, pos.z]);
}
