fn ae2_intrinsic(this: &crate::item_data::ItemStack, id: &str) -> core::ops::ControlFlow<u32> {
    let want = match &*this.item {
        "ae2:fluix_sword" => "minecraft:looting",
        "ae2:fluix_pickaxe" | "ae2:fluix_axe" | "ae2:fluix_hoe" | "ae2:fluix_spade" | "ae2:fluix_shovel" => "minecraft:fortune",
        _ => return core::ops::ControlFlow::Continue(()),
    };
    if id == want && this.enchantments().iter().all(|(e, _)| &**e != id) {
        return core::ops::ControlFlow::Break(1);
    }
    core::ops::ControlFlow::Continue(())
}
