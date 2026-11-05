fn ae2_bus_signal_source(level: &crate::game::GameLevel<'_>, state: horizon_world::registry::BlockStateId, source: &mut bool) {
    // CableBusBlock.isSignalSource: a cell may hold an emitter, so redstone looks at it.
    let name: &str = &level.facts.get(state).name;
    if name.starts_with("ae2:hz_fcl") || name.starts_with("ae2:hz_fcb") {
        *source = true;
    }
}
