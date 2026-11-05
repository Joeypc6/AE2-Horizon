// A level's first offers. The engine finds the set in the world's tables, then rolls it from the resource root's,
// which have no mod's trade sets: the researcher's came out empty. The same body, on the world's tables.
fn ae2_researcher_offers(game: &mut crate::game::Game<'_>, set: &str, variant: &str, origin: [f64; 3]) -> core::ops::ControlFlow<Vec<MerchantOffer>> {
    if !set.starts_with("ae2:") {
        return core::ops::ControlFlow::Continue(());
    }
    let Some(sequence) = game.stations().trades.sequence(set) else {
        return core::ops::ControlFlow::Break(Vec::new());
    };
    let mut random = game.rand_sequences.lend(&sequence, game.level.seed);
    let next_map = game.peek_map_id();
    let tag = |t: &str| game.structure_tag(t);
    let maps = GameMaps {
        terrain: game.end.locator(),
        structure_tag: &tag,
        made: Default::default(),
        next_id: std::cell::Cell::new(next_map),
    };
    let ctx = TradeContext {
        items: game.items(),
        enchantments: &game.stations().enchantments,
        variant,
        origin,
        maps: Some(&maps),
    };
    let offers = game.stations().trades.offers_from_set(&ctx, set, &mut random);
    let made = maps.made.into_inner();
    game.rand_sequences.give_back(sequence, random);
    game.register_exploration_maps(made);
    core::ops::ControlFlow::Break(offers)
}
