// VillagerData's profession is the registry's id, ae2:fluix_researcher (InitVillager.ID).
fn ae2_researcher_nbt_write(m: &Mob) -> core::ops::ControlFlow<Tag> {
    if m.extra.variant2 != 15 {
        return core::ops::ControlFlow::Continue(());
    }
    let types = &crate::mobs::render::VILLAGER_TYPES;
    core::ops::ControlFlow::Break(Tag::compound([
        ("type", string(&format!("minecraft:{}", types[m.extra.variant as usize % types.len()]))),
        ("profession", string("ae2:fluix_researcher")),
        ("level", Tag::Int(i32::from(m.extra.variant3.max(1)))),
    ]))
}
