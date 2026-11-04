fn ae2_clicked_outside(game: &crate::game::Game<'_>, batch: &HudBatch) -> core::ops::ControlFlow<bool> {
    if crate::game::port_ae2_me::clicked_widget(game, batch) {
        return core::ops::ControlFlow::Break(false);
    }
    core::ops::ControlFlow::Continue(())
}
