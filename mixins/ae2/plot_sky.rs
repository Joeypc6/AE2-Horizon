// spatial_storage.json: ambient light 1, black fog and sky, no flashes.
fn ae2_plot_sky(game: &crate::game::Game<'_>, sky: &mut horizon_client::environment::Sky) -> core::ops::ControlFlow<()> {
    let inside = crate::game::port_ae2_me::in_plot(game);
    horizon_client_render::mesher::Mesher::hz_ae2_set_in_plot(inside);
    if !inside {
        return core::ops::ControlFlow::Continue(());
    }
    sky.sky_color = [0.0; 3];
    sky.fog_color = [0.0; 3];
    sky.ambient_light_color = [1.0; 3];
    sky.star_brightness = 0.0;
    core::ops::ControlFlow::Break(())
}
