fn ae2_pick_item(game: &mut crate::game::Game<'_>, pos: [i32; 3], from_entity: bool, cancel: &mut bool) {
    crate::game::port_ae2_me::pick_item(game, pos, from_entity, cancel);
}
