fn ae2_random_tick(level: &GameLevel<'_>, pos: BlockPos, state: BlockStateId, handled: &mut bool) {
    crate::game::port_ae2_me::random_tick(&level.facts.get(state).name, [pos.x, pos.y, pos.z], handled);
}
