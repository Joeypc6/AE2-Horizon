fn ae2_bus_bake(me: &mut Mesher<'_>, region: &[[Option<&Chunk>; 3]; 3], sy: i32, scratch: &mut MeshScratch, value: SectionMesh) -> SectionMesh {
    let mut out = value;
    let center = region[1][1].expect("centre chunk");
    // An all-air section returned before its lighting was padded, and the table is a moment behind: no cell here.
    if center.sections[sy as usize].is_uniform(0) {
        return out;
    }
    let Ok(table) = hz_ae2_cells::SECTIONS.read() else { return out };
    let Some(pieces) = table.as_ref().and_then(|t| t.get(&(center.x, sy, center.z))) else { return out };
    let pad = &scratch.padded;
    let base_y = center.min_y() + sy * 16;
    let mut tints = Tints::new(region, me.biomes, me.zoom, me.blend_radius, [center.x, center.z, base_y]);
    let catalog = &me.render.model_catalog;
    let mut models = Vec::new();
    for (at, id, shift, clip) in pieces {
        let world = BlockPos::new(center.x * 16 + at[0], base_y + at[1], center.z * 16 + at[2]);
        if catalog.models_at(BlockStateId(*id), world, &mut models).is_err() {
            continue;
        }
        let state = &me.tables.info[*id as usize];
        let cell = hz_ae2_cells::padded_cell(*at);
        let origin = [at[0] as f32 + shift[0], at[1] as f32 + shift[1], at[2] as f32 + shift[2]];
        for model in &models {
            for face in &model.faces {
                match clip {
                    Some(b) => {
                        if let Some(cut) = hz_ae2_cells::clipped(face, b) {
                            me.face(pad, &mut tints, &mut out, *at, cell, origin, &cut, false, (*id, state), world, false);
                        }
                    }
                    None => me.face(pad, &mut tints, &mut out, *at, cell, origin, face, false, (*id, state), world, false),
                }
            }
        }
    }
    out.finish_sprite_layers();
    out
}
