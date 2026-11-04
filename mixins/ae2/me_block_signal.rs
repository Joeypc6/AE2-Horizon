fn ae2_block_signal(pos: horizon_core::position::BlockPos, signal: &mut i32) {
    crate::game::port_ae2_me::block_signal([pos.x, pos.y, pos.z], signal);
}
