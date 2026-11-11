fn kit_guide_tooltip_ae2(stack: &horizon_client::items::ItemStack, lines: &mut Vec<(horizon_client::text::Text, u32)>) {
    ae2_optional::if_kit!("ae2", {
    crate::game::hz_kit_guide_ae2::tooltip(stack, lines);
} else { let _ = (stack, lines); () })
}
