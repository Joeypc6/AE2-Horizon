fn ae2_can_place(c: &Containers<'_>, view: &View, slot: usize, stack: &ItemStack) -> core::ops::ControlFlow<bool> {
    if view.kind == Kind::Mod
        && let Some(BlockEntity::Mod(m)) = c.world.block_entities.get(&view.parts[0].0)
    {
        if &*m.id == "ae2:charger" {
            return core::ops::ControlFlow::Break(slot == 0 && crate::game::port_ae2_me::charger_can_place(m, &stack.item));
        }
        if &*m.id == "ae2:inscriber" {
            return core::ops::ControlFlow::Break(crate::game::port_ae2_me::inscriber_can_place(m, slot, &stack.item));
        }
        if &*m.id == "ae2:molecular_assembler" {
            return core::ops::ControlFlow::Break(crate::game::port_ae2_me::assembler_can_place(view.parts[0].0, m, slot, &stack.item));
        }
        if &*m.id == "ae2:vibration_chamber" {
            return core::ops::ControlFlow::Break(slot == 0 && c.items.get(&stack.item).is_some_and(|i| i.fuel.is_some()));
        }
        if let Some(can) = crate::game::port_ae2_me::automation_can_place(&m.id, slot, &stack.item) {
            return core::ops::ControlFlow::Break(can);
        }
    }
    core::ops::ControlFlow::Continue(())
}
