// VillagerData's profession goes by vanilla's registry ids, which have no researcher: it travels as level + 16, over
// "none". mob_meta has just run; researcher_net_decode.rs reads it back.
fn ae2_researcher_net_encode(m: &Mob, meta: &mut Vec<(u8, i32, Value)>) {
    if m.extra.variant2 != 15 || !matches!(m.species(), Species::Villager | Species::ZombieVillager) {
        return;
    }
    for (_, kind, value) in meta.iter_mut() {
        if *kind == serializer::VILLAGER_DATA
            && let Value::Ids(ids) = value
            && let [_, _, level] = ids.as_mut_slice()
            && *level < 16
        {
            *level += 16;
        }
    }
}
