// InitVillager.POI_KEY: one ticket, in the world's poi files.
fn ae2_researcher_poi_type(state: &horizon_world::chunk::BlockState) -> core::ops::ControlFlow<Option<(&'static str, i32)>> {
    if &*state.name == "ae2:charger" {
        core::ops::ControlFlow::Break(Some(("ae2:fluix_researcher", 1)))
    } else {
        core::ops::ControlFlow::Continue(())
    }
}
