fn ae2_can_place(c: &Containers<'_>, view: &View, slot: usize, stack: &ItemStack) -> core::ops::ControlFlow<bool> {
    if view.kind == Kind::Mod
        && let Some(BlockEntity::Mod(m)) = c.world.block_entities.get(&view.parts[0].0)
    {
        if &*m.id == "ae2:inscriber" {
            return core::ops::ControlFlow::Break(crate::game::port_ae2_me::inscriber_can_place(m, slot, &stack.item));
        }
        if &*m.id == "ae2:molecular_assembler" {
            return core::ops::ControlFlow::Break(crate::game::port_ae2_me::assembler_can_place(view.parts[0].0, m, slot, &stack.item));
        }
    }
    core::ops::ControlFlow::Continue(())
}
