// A spatial plot isn't the End: no "The End?" for going in, none for coming back.
fn ae2_plot_travel(to: Dim, at: [f64; 3]) -> core::ops::ControlFlow<()> {
    if crate::game::port_ae2_me::plot_travel(to == Dim::End, at) {
        return core::ops::ControlFlow::Break(());
    }
    core::ops::ControlFlow::Continue(())
}
