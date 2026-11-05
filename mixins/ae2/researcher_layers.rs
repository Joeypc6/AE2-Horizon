// VillagerProfessionLayer for profession 15: the villager arm of `layers`, with assets/ae2/textures/entity/*/profession/.
fn ae2_researcher_layers(m: &Mob) -> core::ops::ControlFlow<Vec<Layer>> {
    let villager = m.species() == Species::Villager;
    if m.extra.variant2 != 15 || !(villager || m.species() == Species::ZombieVillager) {
        return core::ops::ControlFlow::Continue(());
    }
    let (base, folder) = if villager { ("villager", "entity/villager") } else { ("zombie_villager", "entity/zombie_villager") };
    let model = if m.is_baby() {
        interned(format_args!("{base}_baby#main"))
    } else {
        interned(format_args!("{base}#main"))
    };
    let t = VILLAGER_TYPES[m.extra.variant as usize % VILLAGER_TYPES.len()];
    let group = if m.is_baby() { "baby" } else { "type" };
    let layer = |texture: Cow<'static, str>| Layer {
        model: model.clone(),
        texture,
        color: 0xFFFF_FFFF,
        full_bright: false,
        scroll: 0.0,
    };
    let mut v = vec![layer(interned(format_args!("{folder}/{group}/{t}")))];
    if !m.is_baby() {
        v.push(layer(interned(format_args!("ae2:{folder}/profession/fluix_researcher"))));
        let level = ["stone", "iron", "gold", "emerald", "diamond"][(m.extra.variant3.max(1) as usize - 1).min(4)];
        v.push(layer(interned(format_args!("{folder}/profession_level/{level}"))));
    }
    humanoid_armor(m, &mut v);
    core::ops::ControlFlow::Break(v)
}
