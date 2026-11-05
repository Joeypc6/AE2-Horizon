// The name of InitVillager.ID's path for profession 15, where PROFESSIONS (15 long) reads "none".
fn ae2_researcher_profession(value: &'static str, m: &horizon_client::mobs::Mob) -> &'static str {
    if m.extra.variant2 == 15 && m.species() == horizon_client::mobs::Species::Villager {
        "fluix_researcher"
    } else {
        value
    }
}
