// The translucent pass the block entity drawing can't do, run from `native_world_drawn`: OverlayRenderType's faces and
// lines (OverlayRenderer, the Spatial Anchor's chunks), the see-through facades of CableRenderMode.CABLE_VIEW
// (FacadeBuilder) and RenderBlockOutlineHook's placement preview. It lives in the renderer's crate, where the hook runs,
// and that makes it private to that crate: the game reaches it through the `hz_ae2_*` functions on `RenderNeeds`.
//
// With nothing to draw each hook is one atomic load. While something is drawn, the world's depth is kept
// (`RenderNeeds::world_depth`) and read in the shader rather than bound as the pass's depth: the world can draw at a
// lower resolution than the frame (`WorldDrawn::depth_size`), and a pass's attachments must be one size.

use super::{RenderNeeds, WorldDrawn};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// A quad in the world's coordinates, moved to the camera each frame.
#[derive(Clone, Copy)]
struct Quad {
    corners: [[f64; 3]; 4],
    uv: [[f32; 2]; 4],
    /// Its block sprite: array and layer. An array below 0 is no texture, the colour alone.
    sprite: [i32; 2],
    color: [u8; 4],
    /// What of its alpha it keeps behind the world, 0 to hide it there (RenderBlockOutlineHook.LINES_BEHIND_BLOCK).
    behind: f32,
}

#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct PassVertex {
    pos: [f32; 3],
    uv: [f32; 2],
    sprite: [i32; 2],
    color: [u8; 4],
    behind: f32,
}

const ATTRIBUTES: [wgpu::VertexAttribute; 5] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x2, 2 => Sint32x2, 3 => Unorm8x4, 4 => Float32];

struct Scene {
    /// Until the game replaces them (an anchor's walls).
    kept: Vec<Quad>,
    /// Rebuilt each frame by what asks for them (facades, the preview): the game empties it before the frame is built.
    frame: Vec<Quad>,
    /// Kept for its capacity: no allocation from frame to frame.
    vertices: Vec<PassVertex>,
}

static SCENE: Mutex<Scene> = Mutex::new(Scene { kept: Vec::new(), frame: Vec::new(), vertices: Vec::new() });
static ACTIVE: AtomicBool = AtomicBool::new(false);
static GPU: Mutex<Option<Gpu>> = Mutex::new(None);

impl RenderNeeds {
    /// Empties the quads kept from frame to frame, or this frame's.
    pub fn hz_ae2_clear(kept: bool) {
        if !ACTIVE.load(Ordering::Relaxed) {
            return;
        }
        let mut scene = SCENE.lock().unwrap_or_else(|e| e.into_inner());
        if kept {
            scene.kept.clear();
        } else {
            scene.frame.clear();
        }
        ACTIVE.store(!scene.kept.is_empty() || !scene.frame.is_empty(), Ordering::Relaxed);
    }

    /// Adds a quad, kept from frame to frame or for this frame.
    pub fn hz_ae2_quad(kept: bool, corners: [[f64; 3]; 4], uv: [[f32; 2]; 4], sprite: [i32; 2], color: [u8; 4], behind: f32) {
        let mut scene = SCENE.lock().unwrap_or_else(|e| e.into_inner());
        let quad = Quad { corners, uv, sprite, color, behind };
        if kept {
            scene.kept.push(quad);
        } else {
            scene.frame.push(quad);
        }
        ACTIVE.store(true, Ordering::Relaxed);
    }
}

// `at.z` and the world's depth are reversed, as `WorldDrawn` documents: 0 is the sky, nearer is larger. The small factor
// keeps a quad lying on a surface from flickering with it (a share of the distance, so it stays small nearby).
const SHADER: &str = "
struct Globals {
    view_proj: mat4x4<f32>,
    depth_scale: vec2<f32>,
}
@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var world_depth: texture_depth_2d;
@group(0) @binding(2) var sprite_sampler: sampler;
@group(0) @binding(3) var sprites: texture_2d_array<f32>;
@group(0) @binding(4) var sprites_1: texture_2d_array<f32>;
@group(0) @binding(5) var sprites_2: texture_2d_array<f32>;
@group(0) @binding(6) var sprites_3: texture_2d_array<f32>;
@group(0) @binding(7) var sprites_4: texture_2d_array<f32>;
@group(0) @binding(8) var sprites_5: texture_2d_array<f32>;
@group(0) @binding(9) var sprites_6: texture_2d_array<f32>;
@group(0) @binding(10) var sprites_7: texture_2d_array<f32>;

struct VertexOut {
    @builtin(position) at: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) sprite: vec2<i32>,
    @location(2) color: vec4<f32>,
    @location(3) @interpolate(flat) behind: f32,
}

@vertex
fn vs(@location(0) pos: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) sprite: vec2<i32>, @location(3) color: vec4<f32>, @location(4) behind: f32) -> VertexOut {
    var out: VertexOut;
    out.at = globals.view_proj * vec4<f32>(pos, 1.0);
    out.uv = uv;
    out.sprite = sprite;
    out.color = color;
    out.behind = behind;
    return out;
}

fn sprite_texel(which: i32, uv: vec2<f32>, layer: i32) -> vec4<f32> {
    var texel: vec4<f32>;
    switch which {
        case 1: { texel = textureSampleLevel(sprites_1, sprite_sampler, uv, layer, 0.0); }
        case 2: { texel = textureSampleLevel(sprites_2, sprite_sampler, uv, layer, 0.0); }
        case 3: { texel = textureSampleLevel(sprites_3, sprite_sampler, uv, layer, 0.0); }
        case 4: { texel = textureSampleLevel(sprites_4, sprite_sampler, uv, layer, 0.0); }
        case 5: { texel = textureSampleLevel(sprites_5, sprite_sampler, uv, layer, 0.0); }
        case 6: { texel = textureSampleLevel(sprites_6, sprite_sampler, uv, layer, 0.0); }
        case 7: { texel = textureSampleLevel(sprites_7, sprite_sampler, uv, layer, 0.0); }
        default: { texel = textureSampleLevel(sprites, sprite_sampler, uv, layer, 0.0); }
    }
    return texel;
}

@fragment
fn fs(input: VertexOut) -> @location(0) vec4<f32> {
    var color = input.color;
    let last = vec2<i32>(textureDimensions(world_depth)) - vec2<i32>(1);
    let world = textureLoad(world_depth, min(vec2<i32>(input.at.xy * globals.depth_scale), last), 0);
    if input.at.z * 1.0005 < world {
        if input.behind <= 0.0 {
            discard;
        }
        color.a *= input.behind;
    }
    if input.sprite.x >= 0 {
        let texel = sprite_texel(input.sprite.x, input.uv, input.sprite.y);
        if texel.a < 0.1 {
            discard;
        }
        color *= texel;
    }
    return color;
}
";

/// The pass's wgpu objects, made once (again only if the renderer's device or the frame's format changes).
struct Gpu {
    device: wgpu::Device,
    format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    vertices: wgpu::Buffer,
    capacity: u64,
    /// The bind group, with the views it holds: remade when the world's depth or the sprite arrays are new.
    group: Option<(wgpu::TextureView, wgpu::TextureView, wgpu::BindGroup)>,
}

fn vertex_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ae2 translucent vertices"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl Gpu {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ae2 translucent"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ae2 translucent"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<PassVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &ATTRIBUTES,
                })],
            },
            // OverlayRenderType's NO_CULL.
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::COLOR,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ae2 translucent globals"),
            size: 80,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        const FIRST_CAPACITY: u64 = 64 * 1024;
        Self {
            device: device.clone(),
            format,
            pipeline,
            globals,
            vertices: vertex_buffer(device, FIRST_CAPACITY),
            capacity: FIRST_CAPACITY,
            group: None,
        }
    }
}

/// `native_render_needs`: the world's depth, only while there is something to test against it.
pub(super) fn needs(needs: &mut RenderNeeds) {
    if ACTIVE.load(Ordering::Relaxed) {
        needs.world_depth = true;
    }
}

/// `native_world_drawn`: the quads, over the world.
pub(super) fn draw(world: &mut WorldDrawn<'_>) {
    if !ACTIVE.load(Ordering::Relaxed) || world.block_sprites.views.len() < 8 {
        return;
    }
    let mut scene = SCENE.lock().unwrap_or_else(|e| e.into_inner());
    let Scene { kept, frame, vertices } = &mut *scene;
    vertices.clear();
    let camera = world.camera;
    for quad in kept.iter().chain(frame.iter()) {
        let at = quad.corners.map(|c| [(c[0] - camera.x) as f32, (c[1] - camera.y) as f32, (c[2] - camera.z) as f32]);
        for i in [0, 1, 2, 0, 2, 3] {
            vertices.push(PassVertex { pos: at[i], uv: quad.uv[i], sprite: quad.sprite, color: quad.color, behind: quad.behind });
        }
    }
    if vertices.is_empty() {
        return;
    }
    let mut held = GPU.lock().unwrap_or_else(|e| e.into_inner());
    if !held.as_ref().is_some_and(|g| g.device == *world.device && g.format == world.color_format) {
        *held = Some(Gpu::new(world.device, world.color_format));
    }
    let Some(gpu) = held.as_mut() else { return };
    let bytes: &[u8] = bytemuck::cast_slice(vertices.as_slice());
    if gpu.capacity < bytes.len() as u64 {
        gpu.capacity = bytes.len() as u64 * 2;
        gpu.vertices = vertex_buffer(world.device, gpu.capacity);
    }
    world.queue.write_buffer(&gpu.vertices, 0, bytes);
    let mut globals = [0.0_f32; 20];
    globals[..16].copy_from_slice(&world.view_proj.to_cols_array());
    // The frame's pixels to the world depth's texels: it can be the smaller.
    globals[16] = world.depth_size[0] as f32 / world.size[0].max(1) as f32;
    globals[17] = world.depth_size[1] as f32 / world.size[1].max(1) as f32;
    world.queue.write_buffer(&gpu.globals, 0, bytemuck::cast_slice(&globals));
    let sprites = &world.block_sprites;
    if !gpu.group.as_ref().is_some_and(|g| g.0 == *world.depth && g.1 == sprites.views[0]) {
        let mut entries = vec![
            wgpu::BindGroupEntry { binding: 0, resource: gpu.globals.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(world.depth) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sprites.nearest) },
        ];
        for (i, view) in sprites.views[..8].iter().enumerate() {
            entries.push(wgpu::BindGroupEntry { binding: 3 + i as u32, resource: wgpu::BindingResource::TextureView(view) });
        }
        let group = world.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ae2 translucent"),
            layout: &gpu.pipeline.get_bind_group_layout(0),
            entries: &entries,
        });
        gpu.group = Some((world.depth.clone(), sprites.views[0].clone(), group));
    }
    let Some((_, _, group)) = &gpu.group else { return };
    let count = vertices.len() as u32;
    let used = bytes.len() as u64;
    let mut pass = world.encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("ae2 translucent"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: world.color_view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
        })],
        ..Default::default()
    });
    pass.set_pipeline(&gpu.pipeline);
    pass.set_bind_group(0, group, &[]);
    pass.set_vertex_buffer(0, gpu.vertices.slice(..used));
    pass.draw(0..count, 0..1);
}
