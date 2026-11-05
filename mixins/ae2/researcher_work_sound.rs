// InitVillager.PROFESSION's work sound: SoundEvents.VILLAGER_WORK_LIBRARIAN (class_3417.field_20677).
fn ae2_researcher_work_sound(profession: u8) -> core::ops::ControlFlow<Option<&'static str>> {
    if profession == 15 {
        core::ops::ControlFlow::Break(Some("entity.villager.work_librarian"))
    } else {
        core::ops::ControlFlow::Continue(())
    }
}
