// VillagerProfession.getTrades for InitVillager.PROFESSION, whose sets are under ae2:.
fn ae2_researcher_set_for(profession: &str, level: i32) -> core::ops::ControlFlow<String> {
    if profession.trim_start_matches("minecraft:") == "fluix_researcher" {
        core::ops::ControlFlow::Break(format!("ae2:fluix_researcher/level_{level}"))
    } else {
        core::ops::ControlFlow::Continue(())
    }
}
