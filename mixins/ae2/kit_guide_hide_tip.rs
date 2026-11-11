// `native_tooltip_render_pre`: while the guide is open, the screen behind it shows no tooltip over its pages (the
// guide draws its own items' tooltips itself).
fn kit_guide_hide_tip_ae2(cancel: &mut bool) {
    ae2_optional::if_kit!("ae2", {
    if crate::game::hz_kit_guide_ae2::is_open() {
        *cancel = true;
    }
} else { let _ = cancel; () })
}
