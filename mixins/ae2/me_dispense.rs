fn ae2_dispense(game: &mut crate::game::Game<'_>, pos: [i32; 3], facing: horizon_core::geometry::Direction, stack: &horizon_client::items::ItemStack, answer: &mut Option<crate::game::dispenser::NativeDispensed>) {
    crate::game::port_ae2_me::dispense(game, pos, facing, stack, answer);
}
