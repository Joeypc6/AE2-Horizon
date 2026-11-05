// VillagerData with ae2:fluix_researcher. The body knows names in PROFESSIONS only, so it reads the rest under "none".
fn ae2_researcher_nbt_read(m: &mut Mob, tag: &Tag) -> core::ops::ControlFlow<()> {
    let Some(data) = tag.get("VillagerData") else {
        return core::ops::ControlFlow::Continue(());
    };
    if !text(data.get("profession")).is_some_and(|p| p.trim_start_matches("minecraft:") == "ae2:fluix_researcher") {
        return core::ops::ControlFlow::Continue(());
    }
    let mut plain = data.clone();
    if let Tag::Compound(fields) = &mut plain {
        fields.insert("profession".into(), string("minecraft:none"));
    }
    let mut copy = tag.clone();
    if let Tag::Compound(fields) = &mut copy {
        fields.insert("VillagerData".into(), plain);
    }
    read_villager(m, &copy);
    m.extra.variant2 = 15;
    core::ops::ControlFlow::Break(())
}
