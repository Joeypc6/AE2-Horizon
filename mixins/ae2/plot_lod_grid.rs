// No far terrain over the plots' band.
fn ae2_plot_lod_grid(sampler: &VanillaLodSampler, x: i32, z: i32, side: usize) -> core::ops::ControlFlow<Vec<Column>> {
    // The plots' band: void, with a margin past the far terrain's reach.
    let (from, to) = (1_000_000 >> 4, (1_000_000 + 131_072 + 512 * 64) >> 4);
    if sampler.generator.dimension == "minecraft:the_end" && (from..to).contains(&(x >> 4)) && (from..to).contains(&(z >> 4)) {
        return core::ops::ControlFlow::Break(vec![Column::default(); side * side]);
    }
    core::ops::ControlFlow::Continue(())
}
