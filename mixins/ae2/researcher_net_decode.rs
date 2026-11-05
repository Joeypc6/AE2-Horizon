// researcher_net_encode.rs: a level of 16 or more is the Fluix Researcher.
fn ae2_researcher_net_decode(m: &mut Mob, data: &[i32], builtin: &Builtin) -> core::ops::ControlFlow<()> {
    let &[kind, profession, level] = data else {
        return core::ops::ControlFlow::Continue(());
    };
    if level < 16 {
        return core::ops::ControlFlow::Continue(());
    }
    villager_data(m, &[kind, profession, level - 16], builtin);
    m.extra.variant2 = 15;
    core::ops::ControlFlow::Break(())
}
