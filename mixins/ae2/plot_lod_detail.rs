// No far terrain over the plots' band.
fn ae2_plot_lod_detail(sampler: &VanillaLodSampler, chunks: &[(i32, i32)]) -> core::ops::ControlFlow<()> {
    // The plots' band: void, with a margin past the far terrain's reach.
    let (from, to) = (1_000_000 >> 4, (1_000_000 + 131_072 + 512 * 64) >> 4);
    if sampler.generator.dimension == "minecraft:the_end" && chunks.iter().any(|c| (from..to).contains(&c.0) && (from..to).contains(&c.1)) {
        return core::ops::ControlFlow::Break(());
    }
    core::ops::ControlFlow::Continue(())
}
