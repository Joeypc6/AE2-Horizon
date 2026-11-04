fn ae2_plane_table(
    this: &crate::enchant::Enchantments,
    items: &crate::items::Items,
    value: i32,
    item: &str,
    pool: &[std::sync::Arc<str>],
) -> core::ops::ControlFlow<Vec<(Arc<str>, u32)>> {
    // EnchantmentHooks / CALLING_DAMAGEABLE_FROM_ANVIL: the plane is damageable only in the anvil, so the table never
    // offers it unbreaking (the digger enchantments stay).
    if item == "ae2:annihilation_plane" && pool.iter().any(|e| &**e == "minecraft:unbreaking") {
        let pool: Vec<std::sync::Arc<str>> = pool.iter().filter(|e| &***e != "minecraft:unbreaking").cloned().collect();
        return core::ops::ControlFlow::Break(this.available_results(items, value, item, &pool));
    }
    core::ops::ControlFlow::Continue(())
}
