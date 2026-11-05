// The End's band of spatial plots stays empty, no islands: SpatialStorageChunkGenerator on AE2's own level.
fn ae2_plot_void(terrain: &VanillaTerrain, x: i32, z: i32) -> core::ops::ControlFlow<Proto> {
    // The plots' band: void, with a margin past the far terrain's reach.
    let (from, to) = (1_000_000 >> 4, (1_000_000 + 131_072 + 512 * 64) >> 4);
    if terrain.dimension == "minecraft:the_end" && (from..to).contains(&x) && (from..to).contains(&z) {
        return core::ops::ControlFlow::Break(Box::new(VanillaProto::Failed(x, z)));
    }
    core::ops::ControlFlow::Continue(())
}
