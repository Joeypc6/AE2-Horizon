fn jei_creative_width() -> core::ops::ControlFlow<Option<f32>> {
    ae2_optional::if_kit_jei!("ae2", {
    match crate::game::hz_kit_creative_ae2::screen_width() {
        Some(width) => core::ops::ControlFlow::Break(Some(width)),
        None => core::ops::ControlFlow::Continue(()),
    }
} else { core::ops::ControlFlow::Continue(()) })
}
