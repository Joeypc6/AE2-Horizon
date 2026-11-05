fn jei_no_recipes_line(text: &str) -> core::ops::ControlFlow<()> {
    ae2_optional::if_mod!("jei", {
    if crate::game::port_ae2_jei::chat_allowed(text) {
        core::ops::ControlFlow::Continue(())
    } else {
        core::ops::ControlFlow::Break(())
    }
} else { core::ops::ControlFlow::Continue(()) })
}
