fn ae2_bus_reacts(game: &crate::game::Game<'_>, hit: &super::Hit) -> core::ops::ControlFlow<(InteractionResult, DryUse)> {
    if crate::game::port_ae2_me::part_takes_click(game, hit.pos, hit.point) {
        return core::ops::ControlFlow::Break((InteractionResult::Success, DryUse::default()));
    }
    core::ops::ControlFlow::Continue(())
}
