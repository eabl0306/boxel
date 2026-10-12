//! naga validation of every frozen shader module (skeleton-implemented contract test).
//! Runs on any machine (no GPU): WGSL front end, validator, and every backend wgpu uses on
//! desktop: GLSL (GL tiers), MSL (Metal), HLSL SM 5.1 (FXC) and SM 6.0 (DXC), SPIR-V (Vulkan).
use boxel_shaders::gpu::{
    ChunkGpu, ChunkImmediate, DrawIndexedIndirect, FogHeader, FogLayerGpu, LodImmediate,
    ViewUniform,
};
use boxel_shaders::{ShaderModuleId, Stage};
use naga::back::{glsl, hlsl, msl, spv};
use naga::valid::{Capabilities, ModuleInfo, ValidationFlags, Validator};

fn stage(s: Stage) -> naga::ShaderStage {
    match s {
        Stage::Vertex => naga::ShaderStage::Vertex,
        Stage::Fragment => naga::ShaderStage::Fragment,
        Stage::Compute => naga::ShaderStage::Compute,
    }
}

fn load(id: ShaderModuleId) -> (naga::Module, ModuleInfo) {
    let src = id.source();
    let module = naga::front::wgsl::parse_str(src)
        .unwrap_or_else(|e| panic!("{}: {}", id.label(), e.emit_to_string(src)));
    let caps = if id.uses_immediates() {
        Capabilities::IMMEDIATES
    } else {
        Capabilities::empty()
    };
    let info = Validator::new(ValidationFlags::all(), caps)
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}: {}", id.label(), e.emit_to_string(src)));
    (module, info)
}

#[test]
fn every_module_parses_validates_and_lists_its_entry_points() {
    for id in ShaderModuleId::ALL {
        let (module, _) = load(id);
        for (name, st) in id.entry_points() {
            let ep = module
                .entry_points
                .iter()
                .find(|e| e.name == *name)
                .unwrap_or_else(|| panic!("{}: missing entry point {name}", id.label()));
            assert_eq!(ep.stage, stage(*st), "{}: {name} stage", id.label());
        }
        assert_eq!(
            module.entry_points.len(),
            id.entry_points().len(),
            "{}",
            id.label()
        );
    }
}

fn glsl(
    module: &naga::Module,
    info: &ModuleInfo,
    version: glsl::Version,
    name: &str,
    st: Stage,
) -> Result<String, String> {
    let mut out = String::new();
    // Same writer flags as wgpu-hal's GL backend (it always remaps clip z to [-1, 1]).
    let options = glsl::Options {
        version,
        writer_flags: glsl::WriterFlags::ADJUST_COORDINATE_SPACE,
        ..Default::default()
    };
    let pipeline = glsl::PipelineOptions {
        shader_stage: stage(st),
        entry_point: name.to_string(),
        multiview: None,
    };
    glsl::Writer::new(
        &mut out,
        module,
        info,
        &options,
        &pipeline,
        naga::proc::BoundsCheckPolicies::default(),
    )
    .and_then(|mut w| w.write())
    .map(|_| out)
    .map_err(|e| e.to_string())
}

#[test]
fn glsl_translates_at_each_module_floor() {
    for id in ShaderModuleId::ALL {
        let (module, info) = load(id);
        let versions = match id.min_gl() {
            (3, 3) => [
                glsl::Version::Desktop(330),
                glsl::Version::Embedded {
                    version: 300,
                    is_webgl: false,
                },
            ],
            _ => [
                glsl::Version::Desktop(430),
                glsl::Version::Embedded {
                    version: 310,
                    is_webgl: false,
                },
            ],
        };
        for v in versions {
            for (name, st) in id.entry_points() {
                if let Err(e) = glsl(&module, &info, v, name, *st) {
                    panic!("{}: {name} on {v}: {e}", id.label());
                }
            }
        }
    }
    // Documents why Mid needs GL >= 4.3: GLSL 330 has no SSBOs.
    let (module, info) = load(ShaderModuleId::VoxelPull);
    assert!(
        glsl(
            &module,
            &info,
            glsl::Version::Desktop(330),
            "vs_pull",
            Stage::Vertex
        )
        .is_err()
    );
}

#[test]
fn msl_hlsl_and_spirv_translate() {
    for id in ShaderModuleId::ALL {
        let (module, info) = load(id);
        spv::write_vec(&module, &info, &spv::Options::default(), None)
            .unwrap_or_else(|e| panic!("{}: spir-v: {e}", id.label()));
        for (name, st) in id.entry_points() {
            for lang in [(2u8, 1u8), (2, 4)] {
                let mut per = msl::EntryPointResourceMap::default();
                per.insert(
                    name.to_string(),
                    msl::EntryPointResources {
                        immediates_buffer: id.uses_immediates().then_some(30),
                        ..Default::default()
                    },
                );
                let options = msl::Options {
                    lang_version: lang,
                    per_entry_point_map: per,
                    fake_missing_bindings: true,
                    ..Default::default()
                };
                let pipeline = msl::PipelineOptions {
                    entry_point: Some((stage(*st), name.to_string())),
                    ..Default::default()
                };
                msl::write_string(&module, &info, &options, &pipeline)
                    .unwrap_or_else(|e| panic!("{}: {name} msl {lang:?}: {e}", id.label()));
            }
        }
        for sm in [hlsl::ShaderModel::V5_1, hlsl::ShaderModel::V6_0] {
            let target = |register| hlsl::BindTarget {
                space: 0,
                register,
                ..Default::default()
            };
            // Like wgpu-hal dx12: special constants and immediates in their own registers.
            let options = hlsl::Options {
                shader_model: sm,
                special_constants_binding: Some(target(10)),
                immediates_target: Some(target(11)),
                ..Default::default()
            };
            let mut out = String::new();
            hlsl::Writer::new(
                &mut out,
                &options,
                &hlsl::PipelineOptions { entry_point: None },
            )
            .write(&module, &info, None)
            .unwrap_or_else(|e| panic!("{}: hlsl {sm:?}: {e}", id.label()));
            if id == ShaderModuleId::VoxelPull {
                // DX12 adds the draw's first vertex to SV_VertexID: correct only because every
                // boxel draw uses first_vertex = 0 and base_vertex = 0 (ADR 0010).
                assert!(
                    out.contains("first_vertex"),
                    "naga changed vertex_index lowering on HLSL"
                );
            }
        }
    }
}

#[test]
fn forbidden_draw_builtins_are_never_used() {
    use naga::BuiltIn as B;
    for id in ShaderModuleId::ALL {
        let (module, _) = load(id);
        for ep in &module.entry_points {
            let mut bindings = Vec::new();
            for arg in &ep.function.arguments {
                bindings.extend(arg.binding.clone());
                if let naga::TypeInner::Struct { members, .. } = &module.types[arg.ty].inner {
                    bindings.extend(members.iter().filter_map(|m| m.binding.clone()));
                }
            }
            for b in bindings {
                if let naga::Binding::BuiltIn(bi) = b {
                    assert!(
                        !matches!(
                            bi,
                            B::InstanceIndex
                                | B::DrawIndex
                                | B::NumWorkGroups
                                | B::BaseInstance
                                | B::BaseVertex
                        ),
                        "{}: {} uses {bi:?}",
                        id.label(),
                        ep.name
                    );
                }
            }
        }
    }
}

/// No shader keeps an array in a function-local variable: dynamically indexed, it may live in
/// scratch memory on some drivers (the light curve is indexed in the uniform itself).
#[test]
fn no_function_local_arrays() {
    for id in ShaderModuleId::ALL {
        let (module, _) = load(id);
        let functions = module
            .functions
            .iter()
            .map(|(_, f)| f)
            .chain(module.entry_points.iter().map(|ep| &ep.function));
        for f in functions {
            for (_, local) in f.local_variables.iter() {
                assert!(
                    !matches!(module.types[local.ty].inner, naga::TypeInner::Array { .. }),
                    "{}: {:?} has a local array {:?}",
                    id.label(),
                    f.name,
                    local.name
                );
            }
        }
    }
}

fn const_u32(module: &naga::Module, name: &str) -> u32 {
    let (_, c) = module
        .constants
        .iter()
        .find(|(_, c)| c.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("missing const {name}"));
    match module.global_expressions[c.init] {
        naga::Expression::Literal(naga::Literal::U32(v)) => v,
        ref other => panic!("{name}: unexpected init {other:?}"),
    }
}

fn const_i32(module: &naga::Module, name: &str) -> i32 {
    let (_, c) = module
        .constants
        .iter()
        .find(|(_, c)| c.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("missing const {name}"));
    match module.global_expressions[c.init] {
        naga::Expression::Literal(naga::Literal::I32(v)) => v,
        ref other => panic!("{name}: unexpected init {other:?}"),
    }
}

fn const_f32(module: &naga::Module, name: &str) -> f32 {
    let (_, c) = module
        .constants
        .iter()
        .find(|(_, c)| c.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("missing const {name}"));
    match module.global_expressions[c.init] {
        naga::Expression::Literal(naga::Literal::F32(v)) => v,
        ref other => panic!("{name}: unexpected init {other:?}"),
    }
}

#[test]
fn wgsl_bit_layout_matches_boxel_mesh() {
    use boxel_mesh::layout as l;
    let (module, _) = load(ShaderModuleId::VoxelPull);
    for (name, v) in [
        ("GEO_POS_MASK", l::GEO_POS_MASK),
        ("GEO_Y_SHIFT", l::GEO_Y_SHIFT),
        ("GEO_Z_SHIFT", l::GEO_Z_SHIFT),
        ("GEO_W_SHIFT", l::GEO_W_SHIFT),
        ("GEO_H_SHIFT", l::GEO_H_SHIFT),
        ("GEO_FACE_SHIFT", l::GEO_FACE_SHIFT),
        ("GEO_FLAGS_SHIFT", l::GEO_FLAGS_SHIFT),
        ("FLAG_EMISSIVE", l::FLAG_EMISSIVE),
        ("MAT_LAYER_MASK", l::MAT_LAYER_MASK),
        ("MAT_AO_SHIFT", l::MAT_AO_SHIFT),
        ("MAT_LIGHT_SHIFT", l::MAT_LIGHT_SHIFT),
        ("MAT_LIGHT_MASK", l::MAT_LIGHT_MASK),
        ("GEO_SHAPE_SHIFT", l::GEO_SHAPE_SHIFT),
        ("GEO_SHAPE_MASK", l::GEO_SHAPE_MASK),
        ("SHAPE_TURNED", u32::from(l::SHAPE_TURNED)),
        ("SHAPE_FLUID_CUBE", u32::from(l::SHAPE_FLUID_CUBE)),
        ("GEO_FLUID_INSET_BIT", l::GEO_FLUID_INSET_BIT),
        ("SHAPE_CROSS", u32::from(l::SHAPE_CROSS)),
        ("SHAPE_FLUID_SURFACE", u32::from(l::SHAPE_FLUID_SURFACE)),
        ("SHAPE_FLUID_SLOPE", u32::from(l::SHAPE_FLUID_SLOPE)),
        ("SHAPE_BOX", u32::from(l::SHAPE_BOX)),
        ("GEO_BOX_MASK", l::GEO_BOX_MASK),
        ("BOX_COORD_BITS", l::BOX_COORD_BITS),
        ("BOX_OFFSET_BITS", l::BOX_OFFSET_BITS),
    ] {
        assert_eq!(const_u32(&module, name), v, "{name}");
    }
    for (name, v) in [
        ("FLUID_SURFACE", l::FLUID_SURFACE),
        ("FLUID_INSET", l::FLUID_INSET),
    ] {
        assert_eq!(const_f32(&module, name), v, "{name}");
    }
    // The box index sits where w - 1 and h - 1 are, and indexes every model box.
    assert_eq!(l::GEO_BOX_SHIFT, l::GEO_W_SHIFT);
    assert_eq!(l::GEO_BOX_MASK as usize + 1, boxel_voxel::MAX_MODEL_BOXES);
    assert_eq!(
        boxel_shaders::gpu::VIEW_BOX_CAPACITY,
        boxel_voxel::MAX_MODEL_BOXES
    );
}

#[test]
fn wgsl_light_nibbles_match_light_value() {
    use boxel_voxel::LightValue as L;
    let (module, _) = load(ShaderModuleId::VoxelPull);
    for (name, v) in [
        ("LIGHT_SKY_SHIFT", L::SKY_SHIFT),
        ("LIGHT_RED_SHIFT", L::RED_SHIFT),
        ("LIGHT_GREEN_SHIFT", L::GREEN_SHIFT),
        ("LIGHT_BLUE_SHIFT", L::BLUE_SHIFT),
    ] {
        assert_eq!(const_u32(&module, name), v, "{name}");
    }
    // A palette entry holds 4 LightValues, 2 per word, as boxel_mesh packs them.
    let entry = boxel_mesh::palette_entry([L(0x1234), L(0x5678), L(0x9abc), L(0xdef0)]);
    assert_eq!(entry, [0x5678_1234, 0xdef0_9abc]);
}

#[test]
fn cull_clamps_to_the_index_buffer_of_boxel_mesh() {
    let (module, _) = load(ShaderModuleId::Cull);
    assert_eq!(
        const_u32(&module, "MAX_QUADS_PER_BUCKET"),
        boxel_mesh::MAX_QUADS_PER_BUCKET
    );
    assert_eq!(
        const_u32(&module, "MAX_PAGES"),
        boxel_shaders::gpu::chunk_memory::MAX_PAGES
    );
}

/// Where `cull.wgsl` writes draw `n` (counted from 0) of page `p`'s opaque (`cutout = false`) or
/// cutout list, for `pages` lists of `stride` opaque draws; `None` when the list is full. The
/// same arithmetic as the shader, spelled out below.
fn cull_slot(p: u32, n: u32, cutout: bool, pages: u32, stride: u32) -> Option<u32> {
    let max_pages = boxel_shaders::gpu::chunk_memory::MAX_PAGES;
    let pages = pages.min(max_pages);
    if p >= pages {
        return None;
    }
    if cutout {
        let cut = stride / 6;
        (n < cut).then(|| pages * stride + p * cut + n)
    } else {
        (n < stride).then(|| p * stride + n)
    }
}

/// The per-page draw lists of `cull.wgsl` (ADR 0035): its indexing is the one of [`cull_slot`]
/// (the source spells it out), and for any page count, stride and draw count every write stays
/// inside its own page's list, and the lists of different pages never overlap.
#[test]
fn cull_writes_stay_inside_their_page_lists() {
    use boxel_shaders::gpu::chunk_memory::{MAX_PAGES, PAGE_DRAWS_MAX};
    for line in [
        "let pages = min(view.counts.y, MAX_PAGES);",
        "if (p >= pages) { return; }",
        "let s = atomicAdd(&cull.counts[p], 1u);",
        "if (s < stride) {",
        "let i = p * stride + s;",
        "let cut = stride / 6u;",
        "let c = atomicAdd(&cull.counts[MAX_PAGES + p], 1u);",
        "if (c < cut) {",
        "let i = pages * stride + p * cut + c;",
    ] {
        assert!(boxel_shaders::CULL_WGSL.contains(line), "{line}");
    }
    for (pages, stride) in [(1, 6), (3, 24), (4, PAGE_DRAWS_MAX), (MAX_PAGES + 4, 12)] {
        let total = pages.min(MAX_PAGES) * stride * 7 / 6;
        let mut seen = vec![false; total as usize];
        for p in 0..pages + 2 {
            for cutout in [false, true] {
                let cap = if cutout { stride / 6 } else { stride };
                let (base, end) = if cutout {
                    let start = pages.min(MAX_PAGES) * stride + p * (stride / 6);
                    (start, start + stride / 6)
                } else {
                    (p * stride, p * stride + stride)
                };
                for n in 0..cap + 3 {
                    match cull_slot(p, n, cutout, pages, stride) {
                        Some(i) => {
                            assert!(p < pages.min(MAX_PAGES) && n < cap);
                            assert!((base..end).contains(&i), "page {p} draw {n}: {i}");
                            assert!(i < total);
                            assert!(!seen[i as usize], "two draws at {i}");
                            seen[i as usize] = true;
                        }
                        None => assert!(p >= pages.min(MAX_PAGES) || n >= cap),
                    }
                }
            }
        }
        assert!(
            seen.iter().all(|&s| s),
            "{pages} pages of {stride}: every entry used"
        );
    }
}

fn struct_layout(module: &naga::Module, name: &str) -> (u32, Vec<u32>) {
    let mut layouter = naga::proc::Layouter::default();
    layouter.update(module.to_ctx()).expect("layout");
    let (h, ty) = module
        .types
        .iter()
        .find(|(_, t)| t.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("missing struct {name}"));
    let naga::TypeInner::Struct { members, .. } = &ty.inner else {
        panic!("{name} is not a struct")
    };
    (layouter[h].size, members.iter().map(|m| m.offset).collect())
}

#[test]
fn gpu_struct_layouts_match_the_rust_mirrors() {
    use std::mem::{offset_of, size_of};
    let (pull, _) = load(ShaderModuleId::VoxelPull);
    let (size, offsets) = struct_layout(&pull, "View");
    // The per-frame part, then the model box table (ADR 0020).
    assert_eq!(u64::from(size), boxel_shaders::gpu::VIEW_BUFFER_SIZE);
    assert_eq!(size, 352 + 8192);
    assert_eq!(
        u64::from(offsets[11]),
        boxel_shaders::gpu::VIEW_BOXES_OFFSET
    );
    assert_eq!(size_of::<ViewUniform>(), 352);
    let offsets = offsets[..11].to_vec();
    let rust = [
        offset_of!(ViewUniform, view_proj),
        offset_of!(ViewUniform, frustum),
        offset_of!(ViewUniform, render_origin),
        offset_of!(ViewUniform, camera_rel),
        offset_of!(ViewUniform, fog_color),
        offset_of!(ViewUniform, fog_params),
        offset_of!(ViewUniform, counts),
        offset_of!(ViewUniform, sky_light),
        offset_of!(ViewUniform, light_params),
        offset_of!(ViewUniform, light_curve),
        offset_of!(ViewUniform, viewport),
    ];
    assert_eq!(offsets, rust.map(|o| o as u32));
    assert_eq!(offsets[7..], [240, 256, 272, 336]);
    let (size, offsets) = struct_layout(&pull, "ChunkGpu");
    assert_eq!((size, size_of::<ChunkGpu>()), (64, 64));
    assert_eq!(offsets, [0, 12, 16, 48, 52, 56, 60]);
    assert_eq!(offset_of!(ChunkGpu, flags), 48);
    assert_eq!(offset_of!(ChunkGpu, light_base), 52);
    assert_eq!(offset_of!(ChunkGpu, light_len), 56);
    assert_eq!(offset_of!(ChunkGpu, page), 60);
    let (cull, _) = load(ShaderModuleId::Cull);
    assert_eq!(
        struct_layout(&cull, "DrawArgs").0 as usize,
        size_of::<DrawIndexedIndirect>()
    );
    assert_eq!(size_of::<DrawIndexedIndirect>(), 20);
    // The cave culling bits follow the two draw counts of every page (ADR 0022, ADR 0035).
    let (_, offsets) = struct_layout(&cull, "CullState");
    assert_eq!(
        u64::from(offsets[1]),
        boxel_shaders::gpu::bindings::CULL_VISIBLE_OFFSET
    );
    assert_eq!(
        boxel_shaders::gpu::bindings::CULL_VISIBLE_OFFSET,
        8 * u64::from(boxel_shaders::gpu::chunk_memory::MAX_PAGES)
    );
    let (low, _) = load(ShaderModuleId::VoxelInstanced);
    assert_eq!(
        struct_layout(&low, "ChunkImmediate").0 as usize,
        size_of::<ChunkImmediate>()
    );
    assert_eq!(size_of::<ChunkImmediate>(), 16);
}

#[test]
fn low_tier_has_one_uniform_block_and_one_texture_sampler_pair() {
    let (module, _) = load(ShaderModuleId::VoxelInstanced);
    let mut uniforms = 0;
    let mut textures = 0;
    let mut samplers = 0;
    for (_, g) in module.global_variables.iter() {
        match g.space {
            naga::AddressSpace::Uniform => uniforms += 1,
            naga::AddressSpace::Storage { .. } => panic!("Low tier must not use storage buffers"),
            naga::AddressSpace::Handle => match module.types[g.ty].inner {
                naga::TypeInner::Image { .. } => textures += 1,
                naga::TypeInner::Sampler { .. } => samplers += 1,
                _ => {}
            },
            _ => {}
        }
    }
    assert_eq!((uniforms, textures, samplers), (1, 1, 1));
}

/// The overlay and the block outline (ADR 0026) run on every tier, the Low one included: no
/// global resources at all (no uniform, texture, sampler or storage buffer), everything comes
/// in vertex attributes.
#[test]
fn overlay_and_outline_bind_nothing() {
    for id in [ShaderModuleId::Overlay, ShaderModuleId::Outline] {
        let (module, _) = load(id);
        assert_eq!(module.global_variables.len(), 0, "{id:?}");
        assert!(!id.uses_immediates(), "{id:?}");
    }
}

/// The UI pass (ADR 0033) runs on every tier, the Low one on GL included: one 2D float texture
/// (the atlas page, read with `textureLoad`) and nothing else: no sampler, uniform, storage
/// buffer or immediates. Every fragment input but the position is flat, so no tier interpolates
/// a value the texel or the tint depend on.
#[test]
fn ui_binds_one_texture_and_nothing_else() {
    let id = ShaderModuleId::Ui;
    let (module, _) = load(id);
    assert!(!id.uses_immediates());
    assert_eq!(id.min_gl(), (3, 3));
    let globals: Vec<_> = module.global_variables.iter().map(|(_, g)| g).collect();
    assert_eq!(globals.len(), 1, "one global: the atlas");
    let atlas = globals[0];
    assert_eq!(atlas.space, naga::AddressSpace::Handle);
    assert_eq!(
        atlas.binding,
        Some(naga::ResourceBinding {
            group: 0,
            binding: 0
        })
    );
    assert_eq!(
        module.types[atlas.ty].inner,
        naga::TypeInner::Image {
            dim: naga::ImageDimension::D2,
            arrayed: false,
            class: naga::ImageClass::Sampled {
                kind: naga::ScalarKind::Float,
                multi: false,
            },
        }
    );
    let fs = module
        .entry_points
        .iter()
        .find(|e| e.name == "fs_ui")
        .expect("fs_ui");
    let mut inputs = Vec::new();
    for arg in &fs.function.arguments {
        let naga::TypeInner::Struct { members, .. } = &module.types[arg.ty].inner else {
            panic!("fs_ui takes the vertex output struct");
        };
        for member in members {
            match member.binding.as_ref().expect("bound member") {
                naga::Binding::BuiltIn(b) => {
                    assert_eq!(*b, naga::BuiltIn::Position { invariant: false });
                }
                naga::Binding::Location {
                    location,
                    interpolation,
                    ..
                } => {
                    assert_eq!(
                        *interpolation,
                        Some(naga::Interpolation::Flat),
                        "location {location} is not flat"
                    );
                    inputs.push(*location);
                }
            }
        }
    }
    assert_eq!(inputs, [0, 1, 2]);
    // The quad comes as four instance attributes of one buffer: rect, tint, scale, source.
    let vs = module
        .entry_points
        .iter()
        .find(|e| e.name == "vs_ui")
        .expect("vs_ui");
    let attributes = vs
        .function
        .arguments
        .iter()
        .filter(|arg| matches!(arg.binding, Some(naga::Binding::Location { .. })))
        .count();
    assert_eq!(attributes, 4);
}

#[test]
fn low_tier_takes_the_quad_and_its_light_as_two_instance_attributes() {
    let (module, _) = load(ShaderModuleId::VoxelInstanced);
    let vs = module
        .entry_points
        .iter()
        .find(|e| e.name == "vs_instanced")
        .expect("vs_instanced");
    let mut locations: Vec<(u32, naga::TypeInner)> = vs
        .function
        .arguments
        .iter()
        .filter_map(|arg| match arg.binding {
            Some(naga::Binding::Location { location, .. }) => {
                Some((location, module.types[arg.ty].inner.clone()))
            }
            _ => None,
        })
        .collect();
    locations.sort_by_key(|(location, _)| *location);
    let uint2 = naga::TypeInner::Vector {
        size: naga::VectorSize::Bi,
        scalar: naga::Scalar::U32,
    };
    assert_eq!(locations, [(0, uint2.clone()), (1, uint2)]);
    // High and Mid keep one attribute (the DrawRef): the palette is read from the arena.
    let (module, _) = load(ShaderModuleId::VoxelPull);
    let vs = module
        .entry_points
        .iter()
        .find(|e| e.name == "vs_pull")
        .expect("vs_pull");
    let attributes = vs
        .function
        .arguments
        .iter()
        .filter(|arg| matches!(arg.binding, Some(naga::Binding::Location { .. })))
        .count();
    assert_eq!(attributes, 1);
}

/// The fog uniform (ADR 0030) as naga lays it out: header and four layers, within the 16 KiB
/// GL 3.3 guarantees for one uniform block.
#[test]
fn fog_uniform_layout_matches_the_rust_mirrors() {
    use boxel_shaders::gpu::{FOG_BUFFER_SIZE, FOG_LAYERS_OFFSET};
    use std::mem::{offset_of, size_of};
    let (module, _) = load(ShaderModuleId::Fog);
    let (size, offsets) = struct_layout(&module, "Fog");
    assert_eq!(u64::from(size), FOG_BUFFER_SIZE);
    const { assert!(FOG_BUFFER_SIZE <= 16384) };
    assert_eq!(size_of::<FogHeader>(), 112);
    let header = [
        offset_of!(FogHeader, ray0),
        offset_of!(FogHeader, ray_x),
        offset_of!(FogHeader, ray_y),
        offset_of!(FogHeader, eye),
        offset_of!(FogHeader, counts),
        offset_of!(FogHeader, sky),
        offset_of!(FogHeader, debug),
    ];
    assert_eq!(offsets[..7], header.map(|o| o as u32));
    assert_eq!(u64::from(offsets[7]), FOG_LAYERS_OFFSET);
    let (size, offsets) = struct_layout(&module, "FogLayer");
    assert_eq!((size as usize, size_of::<FogLayerGpu>()), (272, 272));
    let layer = [
        offset_of!(FogLayerGpu, band),
        offset_of!(FogLayerGpu, fade),
        offset_of!(FogLayerGpu, grid),
        offset_of!(FogLayerGpu, size),
        offset_of!(FogLayerGpu, lit),
        offset_of!(FogLayerGpu, face),
        offset_of!(FogLayerGpu, soft),
        offset_of!(FogLayerGpu, lattice),
        offset_of!(FogLayerGpu, base_lo),
        offset_of!(FogLayerGpu, base_hi),
        offset_of!(FogLayerGpu, octave2),
        offset_of!(FogLayerGpu, shape),
        offset_of!(FogLayerGpu, wind),
        offset_of!(FogLayerGpu, cells),
        offset_of!(FogLayerGpu, profile_a),
        offset_of!(FogLayerGpu, profile_b),
        offset_of!(FogLayerGpu, walk),
    ];
    assert_eq!(offsets, layer.map(|o| o as u32));
    assert_eq!(
        boxel_shaders::gpu::FOG_MAX_LAYERS,
        boxel_fog::MAX_FOG_LAYERS
    );
}

/// The shader's constants are the reference march's and the uniform's flags.
#[test]
fn fog_constants_match_boxel_fog() {
    use boxel_shaders::gpu::fog;
    let (module, _) = load(ShaderModuleId::Fog);
    assert_eq!(const_u32(&module, "FLAG_HALF"), fog::FLAG_HALF);
    assert_eq!(const_u32(&module, "LAYER_VOLUME"), fog::LAYER_VOLUME);
    assert_eq!(const_u32(&module, "LAYER_UNIFORM"), fog::LAYER_UNIFORM);
    assert_eq!(
        const_u32(&module, "LAYER_SKIP_BOTTOM"),
        fog::LAYER_SKIP_BOTTOM
    );
    assert_eq!(const_u32(&module, "LAYER_SKIP_TOP"), fog::LAYER_SKIP_TOP);
    assert_eq!(const_u32(&module, "HI_MIX"), boxel_fog::noise::HI_MIX);
    assert_eq!(
        const_u32(&module, "UNIFORM_PIECES"),
        boxel_fog::march::UNIFORM_PIECES
    );
    assert_eq!(
        const_f32(&module, "EXIT_TAU"),
        boxel_fog::march::EXIT_TAU_LOG2
    );
    assert_eq!(const_f32(&module, "MIN_DIR"), boxel_fog::march::MIN_DIR);
    assert_eq!(
        const_u32(&module, "HAZE_KNOTS") as usize,
        boxel_fog::HAZE_KNOTS
    );
    assert_eq!(
        const_u32(&module, "HAZE_PIECES"),
        boxel_fog::march::HAZE_PIECES
    );
    assert_eq!(const_f32(&module, "LN_2"), std::f32::consts::LN_2);
    assert_eq!(
        const_f32(&module, "SOFT_PIECE"),
        boxel_fog::march::SOFT_PIECE
    );
    assert_eq!(
        const_u32(&module, "SOFT_MAX_PIECES"),
        boxel_fog::march::SOFT_MAX_PIECES
    );
    assert_eq!(
        const_i32(&module, "EDGE_REACH"),
        boxel_fog::noise::EDGE_REACH
    );
}

/// Per entry point, what the fog pipelines bind: the march one uniform block and one texture,
/// the composite one texture, the debug grid one uniform block; never a sampler, a storage
/// buffer or immediates (every tier, the Low one on GL included, ADR 0011). Every resource of
/// the module has its own group and binding.
#[test]
fn fog_entry_points_bind_one_block_and_one_texture_at_most() {
    let (module, info) = load(ShaderModuleId::Fog);
    assert!(!ShaderModuleId::Fog.uses_immediates());
    let mut bindings: Vec<(u32, u32)> = module
        .global_variables
        .iter()
        .filter_map(|(_, g)| g.binding.as_ref().map(|b| (b.group, b.binding)))
        .collect();
    let all = bindings.len();
    bindings.sort_unstable();
    bindings.dedup();
    assert_eq!(bindings.len(), all, "two resources share a binding");
    let mut seen = Vec::new();
    for (i, ep) in module.entry_points.iter().enumerate() {
        let ep_info = info.get_entry_point(i);
        let (mut uniforms, mut textures, mut samplers) = (0, 0, 0);
        for (handle, g) in module.global_variables.iter() {
            if ep_info[handle].is_empty() {
                continue;
            }
            match g.space {
                naga::AddressSpace::Uniform => uniforms += 1,
                naga::AddressSpace::Storage { .. } => panic!("{}: storage buffer", ep.name),
                naga::AddressSpace::Immediate => panic!("{}: immediates", ep.name),
                naga::AddressSpace::Handle => match module.types[g.ty].inner {
                    naga::TypeInner::Image { .. } => textures += 1,
                    naga::TypeInner::Sampler { .. } => samplers += 1,
                    _ => {}
                },
                _ => {}
            }
        }
        seen.push((ep.name.as_str(), (uniforms, textures, samplers)));
    }
    seen.sort_unstable();
    assert_eq!(
        seen,
        [
            ("fs_fog_composite_full", (0, 1, 0)),
            ("fs_fog_composite_half", (0, 1, 0)),
            ("fs_fog_debug_coverage", (1, 0, 0)),
            ("fs_fog_debug_walk", (1, 0, 0)),
            ("fs_fog_march", (1, 1, 0)),
            ("vs_fullscreen", (0, 0, 0)),
        ]
    );
}

/// No module that runs on GL 3.3 selects between integer or bool vectors: naga writes
/// `mix(uvec3, uvec3, bvec3)` for it, which GLSL 330 lacks (integer `mix` came with GLSL 4.50)
/// and NVIDIA's GL compiler rejects as ambiguous (the fog march hit it). Float vectors and
/// scalars are fine.
#[test]
fn no_integer_vector_selects_on_gl_33() {
    for id in ShaderModuleId::ALL
        .into_iter()
        .filter(|id| id.min_gl() == (3, 3))
    {
        let (module, info) = load(id);
        let functions = module.functions.iter().map(|(h, f)| (f, &info[h])).chain(
            module
                .entry_points
                .iter()
                .enumerate()
                .map(|(i, ep)| (&ep.function, info.get_entry_point(i))),
        );
        for (f, finfo) in functions {
            for (h, e) in f.expressions.iter() {
                if !matches!(e, naga::Expression::Select { .. }) {
                    continue;
                }
                let ty = finfo[h].ty.inner_with(&module.types);
                if let naga::TypeInner::Vector { scalar, .. } = ty {
                    assert!(
                        scalar.kind == naga::ScalarKind::Float,
                        "{}: {:?} selects a {scalar:?} vector",
                        id.label(),
                        f.name
                    );
                }
            }
        }
    }
}

/// The far LOD pipeline (ADR 0031) runs on every tier, the Low one on GL included: one uniform
/// block (the View) and one texture, no sampler (`textureLoad`), no storage buffer; the node in
/// 16 bytes of immediates, the quad as one instance attribute.
#[test]
fn lod_binds_one_uniform_block_and_one_texture() {
    let (module, info) = load(ShaderModuleId::Lod);
    assert!(ShaderModuleId::Lod.uses_immediates());
    assert_eq!(ShaderModuleId::Lod.min_gl(), (3, 3));
    let mut seen = Vec::new();
    for (i, ep) in module.entry_points.iter().enumerate() {
        let ep_info = info.get_entry_point(i);
        let (mut uniforms, mut textures, mut immediates) = (0, 0, 0);
        for (handle, g) in module.global_variables.iter() {
            if ep_info[handle].is_empty() {
                continue;
            }
            match g.space {
                naga::AddressSpace::Uniform => uniforms += 1,
                naga::AddressSpace::Immediate => immediates += 1,
                naga::AddressSpace::Storage { .. } => panic!("{}: storage buffer", ep.name),
                naga::AddressSpace::Handle => match module.types[g.ty].inner {
                    naga::TypeInner::Image { .. } => textures += 1,
                    naga::TypeInner::Sampler { .. } => panic!("{}: a sampler", ep.name),
                    _ => {}
                },
                _ => {}
            }
        }
        seen.push((ep.name.as_str(), (uniforms, textures, immediates)));
    }
    seen.sort_unstable();
    assert_eq!(
        seen,
        [
            ("fs_lod", (1, 1, 0)),
            ("fs_lod_water", (1, 1, 0)),
            ("fs_lod_water_depth", (0, 0, 0)),
            ("vs_lod", (1, 0, 1)),
            ("vs_lod_water", (1, 0, 1))
        ]
    );
    assert!(invariant_position(&module, "vs_lod_water"));
    assert!(!invariant_position(&module, "vs_lod"));
    assert_eq!(
        struct_layout(&module, "LodImmediate").0 as usize,
        size_of::<LodImmediate>()
    );
    assert_eq!(
        size_of::<LodImmediate>() as u32,
        boxel_shaders::gpu::lod::IMMEDIATE_SIZE
    );
    // One instance-step attribute: the quad.
    let vs = module
        .entry_points
        .iter()
        .find(|e| e.name == "vs_lod")
        .expect("vs_lod");
    let attributes = vs
        .function
        .arguments
        .iter()
        .filter(|arg| matches!(arg.binding, Some(naga::Binding::Location { .. })))
        .count();
    assert_eq!(attributes, 1);
}

/// Every `textureLoad` mip level is an `i32` (naga GLSL bug gfx-rs/wgpu#10037).
#[test]
fn texture_load_levels_are_i32() {
    for id in ShaderModuleId::ALL {
        let (module, info) = load(id);
        let functions = module.functions.iter().map(|(h, f)| (f, &info[h])).chain(
            module
                .entry_points
                .iter()
                .enumerate()
                .map(|(i, ep)| (&ep.function, info.get_entry_point(i))),
        );
        for (f, finfo) in functions {
            for (_, e) in f.expressions.iter() {
                if let naga::Expression::ImageLoad {
                    level: Some(level), ..
                } = e
                {
                    let ty = finfo[*level].ty.inner_with(&module.types);
                    assert_eq!(
                        ty.scalar(),
                        Some(naga::Scalar::I32),
                        "{}: {:?} loads a mip level of {ty:?}",
                        id.label(),
                        f.name
                    );
                }
            }
        }
    }
}

/// The bits of `lod.wgsl` equal the Rust mirror's, and a quad's octant there is
/// `boxel_mesh::lod::octant_of`.
#[test]
fn lod_bits_match_the_mirror_and_boxel_mesh() {
    use boxel_mesh::PackedQuad;
    use boxel_shaders::gpu::lod as b;
    use boxel_voxel::Face;
    let (module, _) = load(ShaderModuleId::Lod);
    for (name, v) in [
        ("LOD_LEVEL_MASK", b::LEVEL_MASK),
        ("LOD_MIP_SHIFT", b::MIP_SHIFT),
        ("LOD_MIP_MASK", b::MIP_MASK),
        ("LOD_TINT_BIT", b::TINT_BIT),
        ("LOD_OCTANT_X_SHIFT", b::OCTANT_SHIFTS[0]),
        ("LOD_OCTANT_Y_SHIFT", b::OCTANT_SHIFTS[1]),
        ("LOD_OCTANT_Z_SHIFT", b::OCTANT_SHIFTS[2]),
    ] {
        assert_eq!(const_u32(&module, name), v, "{name}");
    }
    // A quad's seam and darkness are where boxel_mesh puts them in its light palette index.
    assert_eq!(
        const_u32(&module, "LOD_SEAM_SHIFT"),
        boxel_mesh::layout::MAT_LIGHT_SHIFT
    );
    assert_eq!(
        const_u32(&module, "LOD_DARK_SHIFT"),
        boxel_mesh::layout::MAT_LIGHT_SHIFT + u32::from(boxel_mesh::lod::DARK_SHIFT)
    );
    assert_eq!(
        const_u32(&module, "LOD_DARK_MASK"),
        u32::from(boxel_mesh::lod::MAX_DARK)
    );
    let q = PackedQuad::new([0; 3], 1, 1, Face::PosY, 7).with_light_index(2 | 13 << 2);
    assert_eq!((q.mat >> const_u32(&module, "LOD_DARK_SHIFT")) & 15, 13);
    assert_eq!(boxel_mesh::lod::dark_of(q), 13);
    // And its bank flag, apart from both.
    let bank = const_u32(&module, "LOD_BANK_BIT");
    assert_eq!(
        bank,
        u32::from(boxel_mesh::lod::BANK_BIT) << boxel_mesh::layout::MAT_LIGHT_SHIFT
    );
    assert_eq!(q.mat & bank, 0);
    let b = q.with_light_index(2 | 13 << 2 | boxel_mesh::lod::BANK_BIT);
    assert!(b.mat & bank != 0 && boxel_mesh::lod::is_bank(b) && !boxel_mesh::lod::is_bank(q));
    assert_eq!(boxel_mesh::lod::dark_of(b), 13);
    assert_eq!(boxel_mesh::lod::seam_of(b), boxel_mesh::lod::Seam::Middle);
    // Every light palette index a LOD quad can have is in the LOD palette.
    let top = 3
        | u16::from(boxel_mesh::lod::MAX_DARK) << boxel_mesh::lod::DARK_SHIFT
        | boxel_mesh::lod::BANK_BIT;
    assert!(usize::from(top) < boxel_mesh::lod::LOD_PALETTE.len());
    // A bank lies behind anything a LOD level draws (the horizon of the largest preset is under
    // 2^16 blocks, the near plane at least 0.01: a depth of 0.01 / 2^16 at worst, kept 64 times
    // apart), in front of the sky (0).
    let depth = const_f32(&module, "LOD_BANK_DEPTH");
    assert!(depth > 0.0 && depth < 0.01 / 65536.0 / 64.0, "{depth}");
    // The flat output of a LOD quad keeps the texture layer, the face (3 bits), the mip, the
    // tint, the emissive flag, the level and the darkness in bits of their own.
    assert_eq!(const_u32(&module, "LOD_OUT_DARK_SHIFT"), 28);
    assert!(const_u32(&module, "LOD_OUT_LEVEL_SHIFT") + 4 <= 28);
    let mat_layer = const_u32(&module, "MAT_LAYER_MASK");
    let face = const_u32(&module, "LOD_OUT_FACE_SHIFT");
    assert!(face >= mat_layer.count_ones());
    assert!(const_u32(&module, "LOD_OUT_MIP_SHIFT") >= face + 3);
    let emissive = const_u32(&module, "LOD_OUT_EMISSIVE_BIT");
    assert!(emissive > const_u32(&module, "LOD_OUT_TINT_BIT"));
    assert!(1 << const_u32(&module, "LOD_OUT_LEVEL_SHIFT") > emissive);
    assert!(
        const_u32(&module, "LOD_OUT_TINT_BIT")
            > b::MIP_MASK << const_u32(&module, "LOD_OUT_MIP_SHIFT")
    );
    assert!(
        1 << const_u32(&module, "LOD_OUT_LEVEL_SHIFT") > const_u32(&module, "LOD_OUT_TINT_BIT")
    );
    // Hidden octants, the mip and the tint do not overlap; the level fits every LOD level.
    assert_eq!(0xFF & (b::MIP_MASK << b::MIP_SHIFT), 0);
    assert_eq!((b::MIP_MASK << b::MIP_SHIFT) & b::TINT_BIT, 0);
    assert_eq!(boxel_mesh::lod::OCTANT_CELLS, 1 << 4);
    for x in [0, 15, 16, 31] {
        for y in [0, 15, 16, 31] {
            for z in [0, 16] {
                for face in Face::ALL {
                    let q = PackedQuad::new([x, y, z], 1, 1, face, 7);
                    let shader = b::OCTANT_SHIFTS
                        .iter()
                        .enumerate()
                        .map(|(i, s)| ((q.geo >> s) & 1) << i)
                        .sum::<u32>();
                    assert_eq!(shader as u8, boxel_mesh::lod::octant_of(q));
                    let want = u8::from(x >= 16) | u8::from(y >= 16) << 1 | u8::from(z >= 16) << 2;
                    assert_eq!(shader as u8, want);
                }
            }
        }
    }
    // The immediates pack as the shader reads them.
    let imm = LodImmediate::new([-1 << 9, 64, 1 << 30], 4, 0b1010_0101, 4, true);
    assert_eq!(imm.node[0] & b::LEVEL_MASK as i32, 4);
    assert_eq!(imm.node[0] & !(b::LEVEL_MASK as i32), -1 << 9);
    assert_eq!((imm.node[1], imm.node[2]), (64, 1 << 30));
    let w = imm.node[3] as u32;
    assert_eq!(
        (
            w & 0xFF,
            (w >> b::MIP_SHIFT) & b::MIP_MASK,
            w & b::TINT_BIT != 0
        ),
        (0b1010_0101, 4, true)
    );
}

/// The GPU-driven far LOD of High (`lod_pull.wgsl`, `lod_cull.wgsl`): the node table and its bits
/// as the Rust mirror has them, the cull's segments within one indirect list and its index
/// counts within the shared index buffer, the geometry in its own group (group 2 stays the main
/// geometry), and the pulled vertex the same function as the instanced one.
#[test]
fn lod_gpu_path_matches_its_mirrors() {
    use boxel_shaders::gpu::{LodNodeGpu, bindings, lod as b};
    use std::mem::offset_of;
    let (pull, pull_info) = load(ShaderModuleId::LodPull);
    let (cull, _) = load(ShaderModuleId::LodCull);
    assert!(!ShaderModuleId::LodPull.uses_immediates());
    assert!(!ShaderModuleId::LodCull.uses_immediates());
    let rust = vec![
        offset_of!(LodNodeGpu, node) as u32,
        offset_of!(LodNodeGpu, quad_base) as u32,
        offset_of!(LodNodeGpu, counts) as u32,
        offset_of!(LodNodeGpu, seams) as u32,
        offset_of!(LodNodeGpu, water) as u32,
        offset_of!(LodNodeGpu, water_seams) as u32,
    ];
    for module in [&pull, &cull] {
        assert_eq!(
            struct_layout(module, "LodNodeGpu"),
            (size_of::<LodNodeGpu>() as u32, rust.clone())
        );
    }
    for (name, v) in [
        ("LOD_LEVEL_MASK", b::LEVEL_MASK),
        ("LOD_HIDDEN_MASK", 0xFF),
        ("LOD_LIVE_BIT", b::LIVE_BIT),
        ("LOD_ACROSS_SHIFT", b::ACROSS_SHIFT),
        ("LOD_SEGMENT_SLOTS", b::SEGMENT_SLOTS),
        ("LOD_SEGMENT_DRAWS", b::SEGMENT_DRAWS),
        ("MAX_QUADS_PER_BUCKET", boxel_mesh::MAX_QUADS_PER_BUCKET),
    ] {
        assert_eq!(const_u32(&cull, name), v, "{name}");
    }
    // The live and across bits are none of the bits the vertex shader reads, nor each other.
    let read = 0xFF | b::MIP_MASK << b::MIP_SHIFT | b::TINT_BIT;
    let across = 0x3F << b::ACROSS_SHIFT;
    assert_eq!((b::LIVE_BIT | across) & read, 0);
    assert_eq!(b::LIVE_BIT & across, 0);
    let ep = cull
        .entry_points
        .iter()
        .find(|e| e.name == "cull_lod")
        .expect("cull_lod");
    assert_eq!(ep.workgroup_size, [b::CULL_WORKGROUP_SIZE, 1, 1]);
    // The occlusion test keeps a margin over the edge growth, and the pyramid's workgroups are
    // what the renderer dispatches.
    let (hiz, _) = load(ShaderModuleId::LodHiz);
    let reduce = hiz
        .entry_points
        .iter()
        .find(|e| e.name == "hiz_reduce")
        .expect("hiz_reduce");
    let e = b::HIZ_WORKGROUP_EDGE;
    assert_eq!(reduce.workgroup_size, [e, e, 1]);
    let margin = const_f32(&cull, "LOD_HIZ_MARGIN");
    assert!(margin > 1.001 && margin < 1.01, "{margin}");
    // vs_lod_pull reads the View (group 1) and the LOD geometry (its own group), nothing else.
    let i = pull
        .entry_points
        .iter()
        .position(|e| e.name == "vs_lod_pull")
        .expect("vs_lod_pull");
    let mut groups: Vec<(u32, u32)> = pull
        .global_variables
        .iter()
        .filter(|(h, _)| !pull_info.get_entry_point(i)[*h].is_empty())
        .filter_map(|(_, g)| g.binding.as_ref().map(|r| (r.group, r.binding)))
        .collect();
    groups.sort_unstable();
    assert_eq!(
        groups,
        [
            (bindings::GROUP_VIEW, 0),
            (b::GROUP_GEOMETRY, 0),
            (b::GROUP_GEOMETRY, 1)
        ]
    );
    // The table entry packs as the shaders read it.
    let imm = LodImmediate::new([-1 << 10, 64, 1 << 30], 5, 0x0F, 4, false);
    let node = LodNodeGpu::new(imm, 1234, [1, 2, 3, 4, 5, 16384], true);
    assert_eq!(node.node[0], imm.node[0]);
    assert_eq!(node.node[3] as u32, imm.node[3] as u32 | b::LIVE_BIT);
    assert_eq!(node.quad_base, 1234);
    let unpacked: Vec<u32> = (0..6)
        .map(|f| (node.counts[f >> 1] >> ((f & 1) * 16)) & 0xFFFF)
        .collect();
    assert_eq!(unpacked, [1, 2, 3, 4, 5, 16384]);
    let dead = LodNodeGpu::new(imm, 0, [0; 6], false);
    assert_eq!(dead.node[3] as u32 & b::LIVE_BIT, 0);
    let seams = [[0, 0], [1, 2], [3, 0], [0, 4], [5, 6], [16384, 1]];
    let seamed = node.with_seams(seams, 0b10_0101);
    assert_eq!(
        seamed.node[3] as u32,
        node.node[3] as u32 | 0b10_0101 << b::ACROSS_SHIFT
    );
    for (f, [face, middle]) in seams.into_iter().enumerate() {
        assert_eq!(seamed.seams[f], face | middle << 16);
    }
    let watered = seamed.with_water(70_000, [9, 16384]);
    assert_eq!(
        (watered.water, watered.water_seams),
        (70_000, 9 | 16384 << 16)
    );
    assert_eq!(watered.seams, seamed.seams);
    // The water entries: the same geometry and View, an invariant position.
    for entry in ["vs_lod_pull_water", "fs_lod_water", "fs_lod_water_depth"] {
        assert!(pull.entry_points.iter().any(|e| e.name == entry), "{entry}");
    }
    assert!(invariant_position(&pull, "vs_lod_pull_water"));
    assert!(!invariant_position(&pull, "vs_lod_pull"));
}

/// True if the vertex entry `entry` of `module` returns a struct whose position is invariant.
fn invariant_position(module: &naga::Module, entry: &str) -> bool {
    let ep = module
        .entry_points
        .iter()
        .find(|e| e.name == entry)
        .expect(entry);
    let ty = ep.function.result.as_ref().expect("a vertex output").ty;
    match &module.types[ty].inner {
        naga::TypeInner::Struct { members, .. } => members.iter().any(|m| {
            matches!(
                m.binding,
                Some(naga::Binding::BuiltIn(naga::BuiltIn::Position {
                    invariant: true
                }))
            )
        }),
        _ => false,
    }
}

/// The High cull's water rule (`water_quads`, mirrored in Rust): the water bucket's middle seam
/// quads are drawn exactly when some drawn octant has its partner across some middle plane
/// hidden, and the node face ones also when some neighbor of the level does not cover its face.
#[test]
fn lod_water_seams_are_drawn_where_a_seam_can_show() {
    use boxel_shaders::gpu::lod::water_quads;
    for hidden in 0..=255u8 {
        let exposed = (0..8u8).any(|o| {
            hidden & 1 << o == 0 && [1u8, 2, 4].iter().any(|&bit| hidden & 1 << (o ^ bit) != 0)
        });
        for across in [0u8, 0b01_0110, 0x3F, 0xFF] {
            let n = water_quads(100, [10, 20], hidden, across);
            let want = match (exposed, across & 0x3F == 0x3F) {
                (true, _) => 100,
                (false, false) => 80,
                (false, true) => 70,
            };
            assert_eq!(n, want, "hidden {hidden:08b}, across {across:06b}");
        }
    }
    assert_eq!(water_quads(5, [10, 20], 0, 0x3F), 0, "never below zero");
}

/// The High cull's seam rule (`bucket_quads`, mirrored in Rust): the middle seam quads of a face
/// bucket are drawn exactly when an octant is drawn while its partner across the middle plane
/// behind that face is hidden, and the node face ones also when the node across does not cover
/// them.
#[test]
fn lod_seams_are_drawn_where_a_seam_can_show() {
    use boxel_shaders::gpu::lod::bucket_quads;
    // Face bucket order +Y, -Y, +X, -X, +Z, -Z: the axis bit of the octant index and the sign.
    let axes = [(2u8, 1), (2, -1), (1, 1), (1, -1), (4, 1), (4, -1)];
    for (f, (bit, sign)) in axes.into_iter().enumerate() {
        for hidden in 0..=255u8 {
            // An octant on the side the face looks away from (its cells are by the middle plane,
            // facing it), drawn, with the one across hidden.
            let exposed = (0..8u8).any(|o| {
                let behind = if sign > 0 { o & bit == 0 } else { o & bit != 0 };
                behind && hidden & 1 << o == 0 && hidden & 1 << (o ^ bit) != 0
            });
            for across in [false, true] {
                let n = bucket_quads(100, [10, 20], f, hidden, across);
                let want = match (exposed, across) {
                    (true, _) => 100,
                    (false, false) => 80,
                    (false, true) => 70,
                };
                assert_eq!(n, want, "face {f}, hidden {hidden:08b}, across {across}");
            }
        }
    }
    assert_eq!(bucket_quads(5, [10, 20], 0, 0, true), 0, "never below zero");
}
