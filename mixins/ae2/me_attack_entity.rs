fn ae2_attack_entity(game: &mut crate::game::Game<'_>, entity: i32, pos: [f64; 3]) {
    crate::game::port_ae2_me::attack_entity(game, entity, pos);
}
