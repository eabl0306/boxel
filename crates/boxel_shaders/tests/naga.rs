//! naga validation of every frozen shader module (skeleton-implemented contract test).
//! Runs on any machine (no GPU): WGSL front end, validator, and every backend wgpu uses on
//! desktop: GLSL (GL tiers), MSL (Metal), HLSL SM 5.1 (FXC) and SM 6.0 (DXC), SPIR-V (Vulkan).
use boxel_shaders::gpu::{ChunkGpu, ChunkImmediate, DrawIndexedIndirect, ViewUniform};
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
        ("SHAPE_LOWER_HALF", u32::from(l::SHAPE_LOWER_HALF)),
        ("SHAPE_UPPER_HALF", u32::from(l::SHAPE_UPPER_HALF)),
        ("SHAPE_CROSS", u32::from(l::SHAPE_CROSS)),
        ("SHAPE_FLUID_SURFACE", u32::from(l::SHAPE_FLUID_SURFACE)),
    ] {
        assert_eq!(const_u32(&module, name), v, "{name}");
    }
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
    assert_eq!(size as usize, size_of::<ViewUniform>());
    assert_eq!(size, 352);
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
    assert_eq!(offsets[..6], [0, 12, 16, 48, 52, 56]);
    assert_eq!(offset_of!(ChunkGpu, flags), 48);
    assert_eq!(offset_of!(ChunkGpu, light_base), 52);
    assert_eq!(offset_of!(ChunkGpu, light_len), 56);
    let (cull, _) = load(ShaderModuleId::Cull);
    assert_eq!(
        struct_layout(&cull, "DrawArgs").0 as usize,
        size_of::<DrawIndexedIndirect>()
    );
    assert_eq!(size_of::<DrawIndexedIndirect>(), 20);
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
