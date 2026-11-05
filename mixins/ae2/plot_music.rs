// A spatial plot has the game's music: getSituationalMusic's End branch needs the End's level.
fn ae2_plot_music(game: &crate::game::Game<'_>) -> core::ops::ControlFlow<(Option<Music>, f32)> {
    if game.title.is_some() || !crate::game::port_ae2_me::in_plot(game) {
        return core::ops::ControlFlow::Continue(());
    }
    let tables = game.res.music();
    let (dimension, biome) = ("minecraft:overworld", "minecraft:the_void");
    let background = tables.background(dimension, biome);
    let situation = Situation {
        screen_music: game.win.is_some().then(horizon_client::music::Music::credits),
        in_world: true,
        in_end: false,
        boss_music: game.boss_music(),
        background: &background,
        probe_volume: tables.volume(dimension, biome),
        creative: game.creative,
        underwater: game.ambience.was_underwater(),
    };
    core::ops::ControlFlow::Break((situation.music(), situation.volume()))
}
