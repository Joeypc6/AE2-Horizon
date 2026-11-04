fn ae2_suck(game: &mut crate::game::Game<'_>, above: Pos, hopper: &View) -> core::ops::ControlFlow<Option<bool>> {
    let to = hopper.parts[0].0;
    match crate::game::port_ae2_me::hopper_pull(game, [above.0, above.1, above.2], [to.0, to.1, to.2]) {
        Some(moved) => core::ops::ControlFlow::Break(Some(moved)),
        None => core::ops::ControlFlow::Continue(()),
    }
}
