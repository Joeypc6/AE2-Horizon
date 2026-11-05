// The trading session of InitVillager.PROFESSION: opened as usual, then its profession (level-ups, the screen's title) and
// random sequences are the researcher's. The body builds them in a macro, out of reach of a hook on the variable.
fn ae2_researcher_session(game: &mut crate::game::Game<'_>, index: usize, item: &str) -> core::ops::ControlFlow<horizon_client::Result<bool>> {
    thread_local! {
        static INSIDE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if INSIDE.get() {
        return core::ops::ControlFlow::Continue(());
    }
    let id = match game.mobs.get(index) {
        Some(m) if m.extra.variant2 == 15 && m.species() == Species::Villager => m.id,
        _ => return core::ops::ControlFlow::Continue(()),
    };
    INSIDE.set(true);
    let result = game.interact_trader(index, item);
    INSIDE.set(false);
    let open = game.station.as_ref().and_then(|s| s.merchant.as_ref()).filter(|s| s.mob == Some(id)).map(|s| s.session.level);
    if let Some(level) = open {
        let profession = "minecraft:fluix_researcher".to_owned();
        let sequences = merchant::Session::sequences_needed(&game.stations().trades, &profession, level);
        let seed = game.level.seed;
        let lent: Vec<_> = sequences.into_iter().map(|seq| { let r = game.rand_sequences.lend(&seq, seed); (seq, r) }).collect();
        if let Some(state) = game.station.as_mut().and_then(|s| s.merchant.as_mut()) {
            state.session.profession = profession;
            for (seq, r) in lent {
                state.session.sequences.insert(seq, r);
            }
        }
    }
    core::ops::ControlFlow::Break(result)
}
