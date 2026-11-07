fn kit_guide_dragged_ae2(delta: [f32; 2], canceled: &mut bool) {
    ae2_optional::if_kit!("ae2", {
    crate::game::hz_kit_guide_ae2::drag(delta, canceled);
} else { () })
}
