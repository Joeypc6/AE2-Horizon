fn ae2_mining_speed(this: &crate::items::Items, held: Option<&crate::items::ItemStack>, block: &str) -> core::ops::ControlFlow<f32> {
    // SkyStoneBreakSpeed.handleBreakFaster: ten times over with a tool better than iron's 6.
    if block != "ae2:sky_stone_block" {
        return core::ops::ControlFlow::Continue(());
    }
    let Some(info) = held.and_then(|s| this.get(&s.item)) else {
        return core::ops::ControlFlow::Continue(());
    };
    let speed = info
        .tool
        .iter()
        .find_map(|rule| rule.speed.filter(|_| rule.blocks.iter().any(|b| this.block_matches(b, block))))
        .unwrap_or(info.tool_default_speed);
    if speed > 6.0 {
        return core::ops::ControlFlow::Break(speed * 10.0);
    }
    core::ops::ControlFlow::Continue(())
}
