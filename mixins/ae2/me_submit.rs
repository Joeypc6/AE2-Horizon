fn ae2_submit(
    game: &crate::game::Game<'_>,
    block: &str,
    state: horizon_world::registry::BlockStateId,
    entity: &horizon_client::containers::ModBlockEntity,
    draw: &mut crate::game::native_block_entity_draw::BlockEntityDraw,
) {
    crate::game::port_ae2_me::submit(game, block, state, entity, draw);
}
