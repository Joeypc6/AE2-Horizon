// InitVillager: PointOfInterestHelper.register(ID, 1, 1, AEBlocks.CHARGER). Profession 15, past render::PROFESSIONS.
fn ae2_researcher_job_site(block: &str) -> core::ops::ControlFlow<Option<u8>> {
    if block == "ae2:charger" {
        core::ops::ControlFlow::Break(Some(15))
    } else {
        core::ops::ControlFlow::Continue(())
    }
}
