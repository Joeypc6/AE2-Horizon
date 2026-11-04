fn ae2_panel_width() -> core::ops::ControlFlow<Option<f32>> {
    match crate::game::port_ae2_me::panel_screen_width() {
        Some(width) => core::ops::ControlFlow::Break(Some(width)),
        None => core::ops::ControlFlow::Continue(()),
    }
}
