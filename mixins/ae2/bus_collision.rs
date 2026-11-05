fn ae2_bus_collision(scene: &SceneCollisions<'_, '_>, p: BlockPos) -> core::ops::ControlFlow<io::Result<VoxelShape>> {
    use crate::containers::BlockEntity;
    let Some(BlockEntity::Mod(m)) = scene.world.block_entities.get(&(p.x, p.y, p.z)) else {
        return core::ops::ControlFlow::Continue(());
    };
    if !m.id.starts_with("ae2:hz_fcl") {
        return core::ops::ControlFlow::Continue(());
    }
    // Built once for each of the entity's changes (CableBusContainer's cached shapes).
    thread_local! {
        static SHAPES: std::cell::RefCell<std::collections::HashMap<(i32, i32, i32), (crate::containers::ModRevision, VoxelShape)>> = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let key = (p.x, p.y, p.z);
    let revision = m.revision();
    if let Some(shape) = SHAPES.with_borrow(|t| t.get(&key).filter(|(r, _)| *r == revision).map(|(_, s)| s.clone())) {
        return core::ops::ControlFlow::Break(Ok(shape));
    }
    let Some(horizon_world::nbt::Tag::IntArray(ints)) = m.nbt.get(&horizon_world::nbt::JavaString::from("shape")) else {
        return core::ops::ControlFlow::Continue(());
    };
    let mut shape: Option<VoxelShape> = None;
    for b in ints.chunks_exact(6) {
        let v = |i: usize| f64::from(b[i]) / 65536.0;
        let Ok(next) = VoxelShape::from_box(Aabb::new(Vec3::new(v(0), v(1), v(2)), Vec3::new(v(3), v(4), v(5)))) else { continue };
        shape = Some(match shape {
            Some(s) => s.join(&next, horizon_core::geometry::ShapeOp::OR).unwrap_or(next),
            None => next,
        });
    }
    let Some(shape) = shape else {
        return core::ops::ControlFlow::Continue(());
    };
    SHAPES.with_borrow_mut(|t| {
        if t.len() > 4096 {
            t.clear();
        }
        t.insert(key, (revision, shape.clone()));
    });
    core::ops::ControlFlow::Break(Ok(shape))
}
