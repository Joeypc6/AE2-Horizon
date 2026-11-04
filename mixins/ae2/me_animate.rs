fn ae2_animate(engine: &mut ParticleEngine, ctx: &AnimateContext<'_>, center: [i32; 3], r: i32) {
    // Block.animateTick of AE2's blocks, for ClientLevel.animateTick's near pass: a block of its own each roll, so the
    // far pass stays vanilla's. Blocks are read as they are; only one of these four goes further.
    if r != 16 {
        return;
    }
    let random = engine.random();
    let x = center[0] + next_int(random, r) - next_int(random, r);
    let y = center[1] + next_int(random, r) - next_int(random, r);
    let z = center[2] + next_int(random, r) - next_int(random, r);
    if !ctx.world.loaded(x, z) {
        return;
    }
    let state = ctx.state([x, y, z]);
    let name: &str = &ctx.facts.get(state).name;
    let Some(name) = name.strip_prefix("ae2:") else { return };
    if !matches!(name, "vibration_chamber" | "charger" | "growth_accelerator" | "quartz_fixture") {
        return;
    }
    // AEConfig's `enableEffects`. This helper is in the client library, out of reach of the game's copy of the config:
    // it takes this one option from the same client.json itself, looking at the file's time every two seconds at most
    // and reading it only when that changed.
    static EFFECTS: std::sync::Mutex<(Option<std::time::Instant>, Option<std::time::SystemTime>, bool)> = std::sync::Mutex::new((None, None, true));
    let effects = {
        let mut e = EFFECTS.lock().unwrap_or_else(|p| p.into_inner());
        if e.0.is_none_or(|at| at.elapsed().as_secs() >= 2) {
            e.0 = Some(std::time::Instant::now());
            let file = std::env::current_dir().unwrap_or_default().join("config").join("ae2").join("client.json");
            let modified = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
            if modified != e.1 {
                // (A file caught half written is looked at again next time.)
                if let Some(doc) = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) {
                    e.1 = modified;
                    e.2 = doc.get("client").and_then(|c| c.get("enableEffects")).and_then(|v| v.as_bool()).unwrap_or(true);
                }
            }
        }
        e.2
    };
    if !effects {
        return;
    }
    let Some(info) = ctx.registry.state(state) else { return };
    let direction = |side: &str| -> [f64; 3] {
        match side {
            "down" => [0.0, -1.0, 0.0],
            "up" => [0.0, 1.0, 0.0],
            "north" => [0.0, 0.0, -1.0],
            "south" => [0.0, 0.0, 1.0],
            "west" => [-1.0, 0.0, 0.0],
            "east" => [1.0, 0.0, 0.0],
            _ => [0.0, 1.0, 0.0],
        }
    };
    let facing = info.properties.get("facing").map_or([0.0, 1.0, 0.0], |f| direction(&f.to_string()));
    let powered = info.properties.get("powered").is_some_and(|v| v == "true");
    // getFront and getTop, for a block that keeps only its facing.
    let top = if facing[1] == 0.0 { [0.0, 1.0, 0.0] } else { [0.0, 0.0, -1.0] };
    let west = [facing[1] * top[2] - facing[2] * top[1], facing[2] * top[0] - facing[0] * top[2], facing[0] * top[1] - facing[1] * top[0]];
    let mid = [f64::from(x) + 0.5, f64::from(y) + 0.5, f64::from(z) + 0.5];
    let lightning = ParticleOptions::named("ae2:lightning_fx");
    match name {
        // VibrationChamberBlock.animateTick: smoke and a flame at its vent, while it burns.
        "vibration_chamber" if powered => {
            let random = engine.random();
            let (ox, oy) = (f64::from(random.next_float()), f64::from(random.next_float()) * 0.2);
            let at: [f64; 3] = std::array::from_fn(|i| mid[i] + facing[i] * 0.6 + top[i] * (-0.3 + oy) + west[i] * (0.3 * ox - 0.15));
            engine.add(&ParticleOptions::simple(T::Smoke), at, [0.0; 3]);
            engine.add(&ParticleOptions::simple(T::Flame), at, [0.0; 3]);
        }
        // ChargerBlock.animateTick: half the time three bolts over what it charges. (AE2's arcs reach for a target,
        // which a mod particle can't; these are its plain lightning at each arc's start.)
        "charger" if engine.random().next_float() >= 0.5 => {
            let holds = match ctx.world.block_entities.get(&(x, y, z)) {
                Some(crate::containers::BlockEntity::Mod(m)) => match m.nbt.get(&horizon_world::nbt::JavaString::from("holds")) {
                    Some(horizon_world::nbt::Tag::String(s)) => s.to_string().unwrap_or_default(),
                    _ => String::new(),
                },
                _ => String::new(),
            };
            if matches!(holds.as_str(), "ae2:certus_quartz_crystal" | "minecraft:book" | "minecraft:compass") {
                for _ in 0..3 {
                    let random = engine.random();
                    let (xo, zo) = (f64::from(random.next_float()) * 0.3 - 0.15, f64::from(random.next_float()) * 0.3 - 0.15);
                    let yo = if random.next_float() < 0.5 { -0.3 } else { 0.3 };
                    engine.add(&lightning, [mid[0] + xo, mid[1] + yo, mid[2] + zo], [0.0; 3]);
                }
            }
        }
        // GrowthAcceleratorBlock.animateTick: lightning off a side with air beside it, while powered.
        "growth_accelerator" if powered => {
            let random = engine.random();
            let (d0, d1) = (f64::from(random.next_float()) - 0.5, f64::from(random.next_float()) - 0.5);
            let (dx, dz, side) = match next_int(random, 4) {
                0 => (0.6, d1, west),
                1 => (d1, 0.6, facing),
                2 => (d1, -0.6, [-facing[0], -facing[1], -facing[2]]),
                _ => (-0.6, d1, [-west[0], -west[1], -west[2]]),
            };
            if ctx.is_air([x + side[0] as i32, y + side[1] as i32, z + side[2] as i32]) {
                let at: [f64; 3] = std::array::from_fn(|i| mid[i] + top[i] * d0 + west[i] * dx + facing[i] * dz);
                engine.add(&lightning, at, [0.0; 3]);
            }
        }
        // QuartzFixtureBlock.animateTick: one roll in fifty, three bolts by its foot.
        "quartz_fixture" if engine.random().next_float() >= 0.98 => {
            let at: [f64; 3] = std::array::from_fn(|i| mid[i] - 0.3 * facing[i]);
            for _ in 0..3 {
                engine.add(&lightning, at, [0.0; 3]);
            }
        }
        _ => {}
    }
}
