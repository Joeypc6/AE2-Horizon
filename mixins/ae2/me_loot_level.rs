fn ae2_loot_level(stack: &LootStack, enchantment: &str) -> core::ops::ControlFlow<i32> {
    // FluixSwordItem's intrinsic Looting, as `me_intrinsic.rs` for tools' Fortune: this read goes by the loot stack.
    if stack.item == "ae2:fluix_sword" && enchantment == "minecraft:looting" && stack.enchantments.iter().all(|(e, _)| e != enchantment) {
        return core::ops::ControlFlow::Break(1);
    }
    core::ops::ControlFlow::Continue(())
}
