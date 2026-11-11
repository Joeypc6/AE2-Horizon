fn kit_guide_shown_ae2(stack: &ItemStack) {
    ae2_optional::if_kit!("ae2", {
    crate::game::hz_kit_guide_ae2::hovered(&stack.item);
} else { let _ = stack; () })
}
