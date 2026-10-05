use wgpu::util::DeviceExt;
const SRC: &str = r#"
struct U { color: vec4<f32>, offset: vec4<f32> };
@group(0) @binding(0) var<uniform> u: U;
struct Out { @builtin(position) pos: vec4<f32>, @location(0) @interpolate(flat) c: u32 };
@vertex fn vs_plain(@builtin(vertex_index) vi: u32) -> Out {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var o: Out; o.pos = vec4(p[vi], 0.5, 1.0); o.c = 0u; return o;
}
@vertex fn vs_uniform(@builtin(vertex_index) vi: u32) -> Out {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var o: Out; o.pos = vec4(p[vi] * u.offset.x, 0.5, 1.0); o.c = 1u; return o;
}
@vertex fn vs_inst(@builtin(vertex_index) vi: u32, @location(0) q: vec2<u32>) -> Out {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var o: Out; o.pos = vec4(p[vi] * f32(q.x), 0.5, 1.0); o.c = q.y; return o;
}
@fragment fn fs(i: Out) -> @location(0) vec4<f32> { return vec4(f32(i.c & 1u), f32((i.c >> 1u) & 1u), 1.0, 1.0) * u.color; }
"#;
pub fn run() {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::GL; desc.flags = wgpu::InstanceFlags::empty();
    let inst = wgpu::Instance::new(desc);
    let a = pollster::block_on(inst.enumerate_adapters(wgpu::Backends::GL)).remove(0);
    println!("{} {}", a.get_info().name, a.get_info().driver_info);
    let (d, q) = pollster::block_on(a.request_device(&Default::default())).unwrap();
    let ub = d.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&[1.0f32, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0]), usage: wgpu::BufferUsages::UNIFORM });
    let vb = d.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytemuck::cast_slice(&[1u32, 3u32]), usage: wgpu::BufferUsages::VERTEX });
    let m = d.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(SRC.into()) });
    let tex = d.create_texture(&wgpu::TextureDescriptor { label: None, size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, view_formats: &[] });
    let view = tex.create_view(&Default::default());
    let rb = d.create_buffer(&wgpu::BufferDescriptor { label: None, size: 64 * 64 * 4, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let attrs = [wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32x2, offset: 0, shader_location: 0 }];
    let dtex = d.create_texture(&wgpu::TextureDescriptor { label: None, size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Depth32Float, usage: wgpu::TextureUsages::RENDER_ATTACHMENT, view_formats: &[] });
    let dview = dtex.create_view(&Default::default());
    for (vs, strip, inst_attr, depth, fmt) in [("vs_plain", false, false, false, wgpu::TextureFormat::Rgba8Unorm), ("vs_plain", false, false, true, wgpu::TextureFormat::Rgba8Unorm), ("vs_plain", false, false, false, wgpu::TextureFormat::Rgba8UnormSrgb), ("vs_inst", true, true, true, wgpu::TextureFormat::Rgba8UnormSrgb)] {
        let tex = d.create_texture(&wgpu::TextureDescriptor { label: None, size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: fmt, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, view_formats: &[] });
        let view = tex.create_view(&Default::default());
        let bufs = [Some(wgpu::VertexBufferLayout { array_stride: 8, step_mode: wgpu::VertexStepMode::Instance, attributes: &attrs })];
        let p = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor { label: None, layout: None,
            vertex: wgpu::VertexState { module: &m, entry_point: Some(vs), compilation_options: Default::default(), buffers: if inst_attr { &bufs } else { &[] } },
            primitive: wgpu::PrimitiveState { topology: if strip { wgpu::PrimitiveTopology::TriangleStrip } else { wgpu::PrimitiveTopology::TriangleList }, ..Default::default() },
            depth_stencil: depth.then(|| wgpu::DepthStencilState { format: wgpu::TextureFormat::Depth32Float, depth_write_enabled: Some(true), depth_compare: Some(wgpu::CompareFunction::Greater), stencil: Default::default(), bias: Default::default() }), multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &m, entry_point: Some("fs"), compilation_options: Default::default(), targets: &[Some(fmt.into())] }), multiview_mask: None, cache: None });
        let bg = d.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &p.get_bind_group_layout(0), entries: &[wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() }] });
        let mut e = d.create_command_encoder(&Default::default());
        { let mut r = e.begin_render_pass(&wgpu::RenderPassDescriptor { label: None, color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })], depth_stencil_attachment: depth.then(|| wgpu::RenderPassDepthStencilAttachment { view: &dview, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0.0), store: wgpu::StoreOp::Store }), stencil_ops: None }), ..Default::default() });
          r.set_pipeline(&p); r.set_bind_group(0, &bg, &[]); if inst_attr { r.set_vertex_buffer(0, vb.slice(..)); } r.draw(0..3, 0..1); }
        e.copy_texture_to_buffer(wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, wgpu::TexelCopyBufferInfo { buffer: &rb, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(64) } }, wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 });
        q.submit([e.finish()]);
        let s = rb.slice(..); s.map_async(wgpu::MapMode::Read, |_| {}); d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let px = s.get_mapped_range().unwrap()[(32 * 64 + 32) * 4..][..4].to_vec(); rb.unmap();
        println!("{vs} strip={strip} depth={depth} fmt={fmt:?}: center px {px:?}");
    }
}

const SRC2: &str = r#"
struct D { origin: vec3<i32>, lod: u32 };
@group(0) @binding(0) var<uniform> d0: D;
@group(1) @binding(0) var<uniform> d1: D;
var<immediate> di: D;
struct Out { @builtin(position) pos: vec4<f32> };
fn tri(vi: u32, o: vec3<i32>) -> Out {
    let p = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    var r: Out; r.pos = vec4(p[vi] * f32(o.x), 0.5, 1.0); return r;
}
@vertex fn vs_g0(@builtin(vertex_index) vi: u32) -> Out { return tri(vi, d0.origin); }
@vertex fn vs_g1(@builtin(vertex_index) vi: u32) -> Out { return tri(vi, d1.origin); }
@vertex fn vs_imm(@builtin(vertex_index) vi: u32) -> Out { return tri(vi, di.origin); }
@vertex fn vs_two(@builtin(vertex_index) vi: u32) -> Out { return tri(vi, d0.origin * d1.origin + vec3<i32>(0)); }
@fragment fn fs() -> @location(0) vec4<f32> { return vec4(1.0, 0.0, 0.0, 1.0); }
@fragment fn fs_uses0() -> @location(0) vec4<f32> { return vec4(1.0, f32(d0.lod), 0.0, 1.0); }
"#;
pub fn run2() {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::GL; desc.flags = wgpu::InstanceFlags::empty();
    let inst = wgpu::Instance::new(desc);
    let a = pollster::block_on(inst.enumerate_adapters(wgpu::Backends::GL)).remove(0);
    println!("{} {} IMMEDIATES={}", a.get_info().name, a.get_info().driver_info, a.features().contains(wgpu::Features::IMMEDIATES));
    let (d, q) = pollster::block_on(a.request_device(&wgpu::DeviceDescriptor { required_features: wgpu::Features::IMMEDIATES, required_limits: wgpu::Limits { max_immediate_size: 16, ..wgpu::Limits::downlevel_webgl2_defaults() }, ..Default::default() })).unwrap();
    let align = d.limits().min_uniform_buffer_offset_alignment as usize;
    let mut bytes = vec![0u8; align * 2];
    bytes[..4].copy_from_slice(&0i32.to_ne_bytes()); // slot 0: origin.x = 0 -> degenerate
    bytes[align..align + 4].copy_from_slice(&1i32.to_ne_bytes()); // slot 1: origin.x = 1 -> fullscreen
    let ub = d.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: &bytes, usage: wgpu::BufferUsages::UNIFORM });
    let m = d.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(SRC2.into()) });
    let tex = d.create_texture(&wgpu::TextureDescriptor { label: None, size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format: wgpu::TextureFormat::Rgba8Unorm, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, view_formats: &[] });
    let view = tex.create_view(&Default::default());
    let rb = d.create_buffer(&wgpu::BufferDescriptor { label: None, size: 64 * 64 * 4, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let ue = |dynamic: bool| wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::VERTEX_FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: dynamic, min_binding_size: wgpu::BufferSize::new(16) }, count: None };
    let bgl_dyn = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[ue(true)] });
    let bgl_static = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[ue(false)] });
    let bg_dyn = d.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl_dyn, entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &ub, offset: 0, size: wgpu::BufferSize::new(16) }) }] });
    let bg_static1 = d.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &bgl_static, entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &ub, offset: align as u64, size: wgpu::BufferSize::new(16) }) }] });
    for case in ["g0_dyn", "g1_dyn", "g0_static", "g1_static", "imm", "two", "two_fs0"] {
        let (vs, dynamic, group) = match case { "g0_dyn" => ("vs_g0", true, 0), "g1_dyn" => ("vs_g1", true, 1), "g0_static" => ("vs_g0", false, 0), "g1_static" => ("vs_g1", false, 1), "two" | "two_fs0" => ("vs_two", true, 7), _ => ("vs_imm", false, 9) };
        let fs = if case == "two_fs0" { "fs_uses0" } else { "fs" };
        let empty = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: None, entries: &[] });
        let bgl = if dynamic { &bgl_dyn } else { &bgl_static };
        let layouts: Vec<Option<&wgpu::BindGroupLayout>> = match group { 0 => vec![Some(bgl)], 1 => vec![Some(&empty), Some(bgl)], 7 => vec![Some(&bgl_static), Some(&bgl_dyn)], _ => vec![] };
        let pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &layouts, immediate_size: if group == 9 { 16 } else { 0 } });
        let p = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor { label: None, layout: Some(&pl),
            vertex: wgpu::VertexState { module: &m, entry_point: Some(vs), compilation_options: Default::default(), buffers: &[] },
            primitive: Default::default(), depth_stencil: None, multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &m, entry_point: Some(fs), compilation_options: Default::default(), targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())] }), multiview_mask: None, cache: None });
        let bg_empty = d.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &empty, entries: &[] });
        let mut e = d.create_command_encoder(&Default::default());
        { let mut r = e.begin_render_pass(&wgpu::RenderPassDescriptor { label: None, color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &view, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store } })], ..Default::default() });
          r.set_pipeline(&p);
          if group == 1 { r.set_bind_group(0, &bg_empty, &[]); }
          if group == 7 { r.set_bind_group(0, &bg_static1, &[]); r.set_bind_group(1, &bg_dyn, &[align as u32]); }
          else if group == 9 { r.set_immediates(0, bytemuck::cast_slice(&[1i32, 0, 0, 0])); }
          else if dynamic { r.set_bind_group(group, &bg_dyn, &[align as u32]); } else { r.set_bind_group(group, &bg_static1, &[]); }
          r.draw(0..3, 0..1); }
        e.copy_texture_to_buffer(wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All }, wgpu::TexelCopyBufferInfo { buffer: &rb, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(64) } }, wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 });
        q.submit([e.finish()]);
        let s = rb.slice(..); s.map_async(wgpu::MapMode::Read, |_| {}); d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let px = s.get_mapped_range().unwrap()[(32 * 64 + 32) * 4..][..4].to_vec(); rb.unmap();
        println!("  {case}: center px {px:?} (expect red)");
    }
}
