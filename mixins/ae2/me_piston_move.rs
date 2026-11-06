fn ae2_piston_move(level: &GameLevel<'_>, push: &[BlockPos], destroy: &[BlockPos], canceled: &mut bool) {
    let held = |p: &BlockPos| crate::game::port_ae2_me::network_stays(&level.facts.get(level.world.block(p.x, p.y, p.z)).name);
    if push.iter().chain(destroy).any(held) {
        *canceled = true;
    }
}
