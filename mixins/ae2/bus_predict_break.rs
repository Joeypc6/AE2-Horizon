fn ae2_bus_predict_break(game: &mut crate::game::Game<'_>, pos: [i32; 3]) -> core::ops::ControlFlow<()> {
    if crate::game::port_ae2_me::keeps_predicted_break(game, pos) {
        game.mining = None;
        return core::ops::ControlFlow::Break(());
    }
    core::ops::ControlFlow::Continue(())
}
