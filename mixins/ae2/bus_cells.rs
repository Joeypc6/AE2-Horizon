// The cells of AE2's cable bus for the mesher: what each chunk section draws besides its blocks (CableBusBakedModel).
use super::*;
use std::collections::HashMap;
use std::sync::RwLock;

pub(super) type Piece = ([i32; 3], u32, [f32; 3], Option<[f32; 6]>);

/// The camera's in a spatial plot: no End sky over it.
pub(crate) static IN_PLOT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(super) static SECTIONS: RwLock<Option<HashMap<(i32, i32, i32), Vec<Piece>>>> = RwLock::new(None);

impl Mesher<'_> {
    pub fn hz_ae2_set_in_plot(inside: bool) {
        IN_PLOT.store(inside, std::sync::atomic::Ordering::Relaxed);
    }

    /// The pieces of a section (chunk x, section index, chunk z), from the game's thread.
    pub fn hz_ae2_set_section(section: (i32, i32, i32), pieces: Vec<([i32; 3], u32, [f32; 3], Option<[f32; 6]>)>) {
        let Ok(mut table) = SECTIONS.write() else { return };
        let table = table.get_or_insert_with(HashMap::new);
        if pieces.is_empty() {
            table.remove(&section);
        } else {
            table.insert(section, pieces);
        }
    }
}

/// A full block's face cut to a facade's plate: corners moved into the box, its UVs with them.
/// A section position's place in the padded arrays, as the mesher's own loop works it out.
pub(super) fn padded_cell(at: [i32; 3]) -> usize {
    pi(at[0], at[1], at[2])
}

pub(super) fn clipped(face: &BakedFace, b: &[f32; 6]) -> Option<BakedFace> {
    let mut q = face.quad.clone();
    let p = face.quad.positions;
    // The two axes the quad spans.
    let axes: Vec<usize> = (0..3).filter(|&k| (0..4).any(|i| (p[i][k] - p[0][k]).abs() > 1e-4)).collect();
    if axes.len() != 2 {
        return None;
    }
    let lo: [f32; 2] = std::array::from_fn(|n| (0..4).map(|i| p[i][axes[n]]).fold(f32::MAX, f32::min));
    let hi: [f32; 2] = std::array::from_fn(|n| (0..4).map(|i| p[i][axes[n]]).fold(f32::MIN, f32::max));
    let corner = |s: f32, t: f32| -> [f32; 2] {
        let near = (0..4)
            .min_by(|&x, &y| {
                let d = |i: usize| (p[i][axes[0]] - s).abs() + (p[i][axes[1]] - t).abs();
                d(x).total_cmp(&d(y))
            })
            .unwrap_or(0);
        face.quad.uv[near]
    };
    let (c00, c10, c01, c11) = (corner(lo[0], lo[1]), corner(hi[0], lo[1]), corner(lo[0], hi[1]), corner(hi[0], hi[1]));
    for i in 0..4 {
        let mut v = p[i];
        for k in 0..3 {
            v[k] = v[k].clamp(b[k], b[k + 3]);
        }
        let s = ((v[axes[0]] - lo[0]) / (hi[0] - lo[0]).max(1e-4)).clamp(0.0, 1.0);
        let t = ((v[axes[1]] - lo[1]) / (hi[1] - lo[1]).max(1e-4)).clamp(0.0, 1.0);
        q.uv[i] = std::array::from_fn(|n| (c00[n] * (1.0 - s) + c10[n] * s) * (1.0 - t) + (c01[n] * (1.0 - s) + c11[n] * s) * t);
        q.positions[i] = v;
    }
    q.cull = None;
    Some(BakedFace::new(q, face.layer, face.opaque, face.translucent, face.fusion))
}
