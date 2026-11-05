fn jei_creative_width() -> core::ops::ControlFlow<Option<f32>> {
    ae2_optional::if_mod!("jei", {
    match crate::game::port_jei_creative::screen_width() {
        Some(width) => core::ops::ControlFlow::Break(Some(width)),
        None => core::ops::ControlFlow::Continue(()),
    }
} else { core::ops::ControlFlow::Continue(()) })
}
