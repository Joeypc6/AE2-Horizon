fn jei_no_recipes_line(text: &str) -> core::ops::ControlFlow<()> {
    ae2_optional::if_kit_jei!("ae2", {
    if crate::game::hz_kit_layer_ae2::chat_allowed(text) {
        core::ops::ControlFlow::Continue(())
    } else {
        core::ops::ControlFlow::Break(())
    }
} else { core::ops::ControlFlow::Continue(()) })
}
