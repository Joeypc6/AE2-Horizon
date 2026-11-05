// A spatial plot has AE2's sky (SpatialSkyRender over a black level), not the End's.
fn ae2_plot_end_sky() -> core::ops::ControlFlow<()> {
    if crate::mesher::hz_ae2_cells::IN_PLOT.load(std::sync::atomic::Ordering::Relaxed) {
        return core::ops::ControlFlow::Break(());
    }
    core::ops::ControlFlow::Continue(())
}
