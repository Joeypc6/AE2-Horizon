// Villager.getTypeName: the jar's lang has entity.minecraft.villager.fluix_researcher.
fn ae2_researcher_name(m: &Mob) -> core::ops::ControlFlow<crate::text::Text> {
    if m.custom_name.is_some() || m.extra.variant2 != 15 || m.species() != Species::Villager {
        return core::ops::ControlFlow::Continue(());
    }
    core::ops::ControlFlow::Break(crate::text::Text::translate("entity.minecraft.villager.fluix_researcher", &[]))
}
