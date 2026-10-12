# boxel: arquitectura

Documento técnico de referencia. Fecha: 2026-10-05. Estado: aprobado para el milestone M0
(revisión final, después de tres revisiones adversariales del borrador v1).
Las decisiones puntuales están en `docs/adr/` (0001 a 0031). Durante M0 hubo además una
especificación para los agentes implementadores (en inglés, en `.scaffold/`), borrada al cerrar M0;
este documento queda como referencia.

Convención: "medido" significa probado en la máquina de desarrollo (Ubuntu 26.04, Wayland,
RTX 5050 Laptop driver 595.91 + Radeon 780M con RADV/radeonsi Mesa 26.0.8, lavapipe/llvmpipe,
Rust 1.98.1). Los números de CPU son ruidosos (carga de 8 a 34 durante algunas mediciones); se
dan como rangos y valen como orden de magnitud.

---

## 1. Resumen

boxel es un motor/librería de vóxeles estilo Bevy, escrito en Rust, solo para escritorio (Linux,
Windows, macOS). Sus piezas:

- **Gráficos:** wgpu 30.0.1 es el único RHI. Encima, una capa propia y delgada (`boxel_gfx`) que
  decide en tiempo de ejecución qué backend, qué adaptador y qué **tier** de render usar, con
  pruebas de humo que usan los pipelines reales y el formato real de la superficie, fallbacks,
  overrides y una memoria de crashes por dispositivo (LKG).
- **ECS:** un fork vendorizado y recortado de `bevy_ecs` 0.20.0 (crates `boxel_ecs*`, cero
  crates `bevy_*` en el árbol), generado por script, con diff de librería cero comprobado en cada
  regeneración.
- **App:** `boxel_app` propio (App, Plugin, schedules, tiempo, task pools, runner), inspirado en
  `bevy_app` pero no es un fork.
- **Vóxeles:** chunks de 32³ con contenedor de paleta estilo Minecraft, snapshots `Arc` con
  copy-on-write, scheduler propio con prioridades y exactamente un resultado por job.
- **Meshing:** binary greedy meshing (`binary-greedy-meshing` 0.5.2) convertido a un `PackedQuad`
  canónico de 8 bytes en **8 buckets** (6 caras opacas, cutout, translúcido). Los mismos bytes
  sirven a todos los tiers.
- **Render:** sin render graph. Por tier, una lista fija de pases. Tres tiers:
  High (culling en GPU + `multi_draw_indexed_indirect_count`), Mid (vertex pulling + culling en
  CPU + draws directos), Low (quads como atributos de instancia + immediates, sin storage
  buffers). Dos pipelines por tier: opaco sin `discard` y cutout con alpha test.
  "Software" no es un tier sino una clase de dispositivo.
- **Ventana:** runner winit 0.30.13 propio (`ApplicationHandler`), `app.update()` desde
  `about_to_wait`; la presentación solo ocurre cuando winit entrega `RedrawRequested` (en Wayland,
  después del frame callback del compositor), así una ventana oculta nunca bloquea la simulación.

Estado validado antes de escribir este documento:

| Adaptador | Backend | High | Mid | Low | Imagen vs High |
|---|---|---|---|---|---|
| RTX 5050 | Vulkan | OK | OK | OK | 0 px distintos |
| Radeon 780M (RADV) | Vulkan | OK | OK | OK | 0 px |
| lavapipe | Vulkan (CPU) | OK (99 ms) | OK (5.5 ms) | OK | 5 px de 230 400 |
| RTX 5050 contexto 3.3 | GL | no aplica | falla (esperado) | OK | idéntica al centro |
| Radeon 780M (radeonsi 4.6) | GL | no aplica | OK | OK | Mid = Low, 0 px |
| llvmpipe 4.5 | GL (CPU) | no aplica | OK | OK | Mid = Low, 0 px |

Entre adaptadores distintos la diferencia máxima medida fue 0.33 % de píxeles. Además, el
esqueleto del M0 compila y pasa sus tests de contrato, clippy con `-D warnings`, rustdoc,
cargo-deny, el MSRV 1.97.1 y los tests del ECS vendorizado (962 + 480) con lockfile fijo.

---

## 2. Objetivos y no objetivos

### Objetivos

1. Detectar al arrancar qué soporta el equipo (Vulkan, GL, DX12, Metal) y elegir backend,
   adaptador y ruta de render **sin perder optimización** donde el hardware la permite.
2. Arquitectura y ergonomía estilo Bevy (ECS, plugins, schedules) sin depender de crates de Bevy.
3. Mundo de bloques estilo Minecraft: chunks, greedy meshing, render GPU-driven, streaming.
4. Dejar la puerta abierta a ray marching y vóxeles más chicos (pool de vóxeles en GPU separado
   de las mallas, pases que componen por profundidad).
5. Robustez de arranque: nunca un crash por un driver raro si existe otra opción usable; cada
   decisión queda registrada y es reproducible con overrides.
6. Calidad de proyecto: CI con tests de GPU en rasterizadores por software, validación de shaders
   con naga en todos los backends, Miri para el ECS vendorizado, lints estrictos.
7. Física propia y mínima para juegos de bloques (ADR 0016): cajas y esferas sin rotaciones,
   gravedad, raycast exacto contra vóxeles, sólidos y sensores entre entidades. Sin motores de
   terceros.

### No objetivos (por ahora)

- Web (WebGPU/WebGL2) y móvil. El diseño no los impide (tiers Mid/Low cubren WebGPU/WebGL2) pero
  no se compila ni se prueba para ellos.
- Un RHI propio o backends crudos de Vulkan/GL (ADR 0001).
- Render graph genérico.
- Editor, assets en disco, audio, red. Fuera del alcance del motor en M0..M2.
- Compatibilidad binaria o de API con Bevy.

---

## 3. Diagrama de capas

```
 +-----------------------------------------------------------------------------------+
 |  Aplicación / juego / ejemplos (voxel_world, headless_world, gpu_info)             |
 +-----------------------------------------------------------------------------------+
 |  boxel (fachada): prelude, DefaultPlugins, MinimalPlugins, features                |
 +----------------------+------------------------+-----------------------------------+
 |                      |  boxel_physics         |                                   |
 |                      |  (barrido, raycast)    |                                   |
 +----------------------+------------------------+-----------------------------------+
 |  boxel_render        |  boxel_world           |  boxel_camera                     |
 |  (plugin + renderer  |  (ChunkMap, streaming, |  (Camera, CameraView,             |
 |   por tier, sin ECS  |   jobs, ediciones,     |   FlyCamera)                      |
 |   en el núcleo)      |   ChunkMeshEvent)      |                                   |
 +----------+-----------+-----------+------------+-------------+---------------------+
 | boxel_   | boxel_shaders         | boxel_mesh  boxel_worldgen| boxel_window        |
 | gfx      | (WGSL + tipos GPU)    | (bgm ->     (generadores) | (runner winit,      |
 | (wgpu:   |                       |  PackedQuad)              |  input)             |
 | detección,                       +------------+--------------+---------------------+
 | tiers,   |                       | boxel_voxel (coords, registry, paleta, ChunkData)  |
 | smoke,   |                       +----------------------------------------------------+
 | LKG)     |                       | boxel_app  boxel_log  boxel_diagnostic             |
 |          |                       | boxel_transform  boxel_math                        |
 +----------+-----------------------+----------------------------------------------------+
 | wgpu 30.0.1 (Vulkan, DX12, Metal, | vendor/: boxel_ecs, boxel_ecs_macros(+logic),      |
 | GL/EGL)  + escape hatch hal       | boxel_macro_utils, boxel_ptr, boxel_tasks,         |
 |                                   | boxel_utils (fork de bevy_ecs 0.20.0)              |
 +-----------------------------------+----------------------------------------------------+
 | winit 0.30.13, glam 0.34.0, binary-greedy-meshing 0.5.2, offset-allocator, slotmap ... |
 +-----------------------------------------------------------------------------------+
```

Reglas de dependencia (verificadas por el esqueleto que compila):

- `boxel_math`, `boxel_voxel`, `boxel_mesh`, `boxel_light`, `boxel_worldgen`, `boxel_gfx`,
  `boxel_shaders` no dependen del ECS. Son puros y testeables sin App.
- Solo `vendor/` depende de código derivado de Bevy. Ningún crate `bevy*` puede entrar al árbol
  (cargo-deny con nombres exactos más `tools/no_bevy.py` para cualquier nombre).
- `boxel_render` depende de `boxel_world` (consume `ChunkMeshEvent`), nunca al revés.
- El motor no trae contenido (ADR 0029). `boxel_demo`, el contenido de los ejemplos, depende solo
  de `boxel_voxel`, `boxel_worldgen`, `boxel_pack` y `boxel_fog`, y los crates del motor lo usan
  solo en tests, benches y ejemplos (`tools/no_content.py`).
- `boxel_fog` (niebla y nubes, ADR 0030) es CPU puro, sin ECS ni GPU y sin dependencias del
  motor: lo usan `boxel_render`, `boxel_shaders` (en tests) y los presets de `boxel_demo`.
- `boxel_physics` depende de `boxel_world` (lee `ChunkMap`); ni `boxel_world` ni `boxel_render`
  dependen de él. Su núcleo (`boxel_physics::core`) no usa el ECS.
- El núcleo del renderer (`VoxelRenderer`) no usa el ECS; el plugin es una capa fina encima.

---

## 4. Mapa de crates

| Crate | Responsabilidad | Depende de (boxel) |
|---|---|---|
| `boxel` (raíz) | Fachada: re-exports, `prelude`, `DefaultPlugins`, `MinimalPlugins`, ejemplos | todos |
| `boxel_app` | `App`, `Plugin`, `PluginGroup`, schedules (`Main`...`Last`, fixed con `RunFixedMainLoopSystems`, `Shutdown`, `Idle` entre frames), `Time`, `FixedTime`, `FrameLimit` y `FramePacer`, task pools, runner headless, `AppExit` | ecs, tasks |
| `boxel_log` | Subscriber de `tracing`, puente de `log`, Tracy opcional | app |
| `boxel_diagnostic` | `FrameStats`, `DiagnosticsBoard`, log periódico | app, ecs |
| `boxel_math` | re-export de glam, `Aabb`, `Frustum`, proyección reverse-Z | (glam) |
| `boxel_transform` | componente `Transform` (traslación f64) | ecs, math |
| `boxel_window` | runner winit, ventana primaria (no-send), mensajes de ventana, input | app, ecs, math |
| `boxel_camera` | `Camera`, `CameraView` (relativa a cámara), `FlyCamera` | app, transform, window |
| `boxel_gfx` | política sobre wgpu: preferencias, instancia, tiers, scoring, smoke, selección, LKG, superficie, readback, hal; ejemplo `gpu_info` | (wgpu) |
| `boxel_shaders` | WGSL embebido, composición, espejos `#[repr(C)]`, tests de naga | (wgpu, bytemuck) |
| `boxel_voxel` | coordenadas (`BlockPos`, `ChunkPos`, `WorldPos`: bloque más offset, exacto a cualquier distancia), `Face`, registry (con emisión RGB y opacidad de luz), paleta, `LightValue` y `LightTables`, `ChunkData` con sus máscaras de luz (`LightMasks`), qué caras de un chunk se ven entre sí (`ChunkFaceInfo`, ADR 0022), clases de LOD y la regla de la celda lejana (`lod`, ADR 0031) | (glam) |
| `boxel_mesh` | `PackedQuad`, `ChunkMesh` con su paleta de luz, `PaddedChunk`, `Mesher` (fork de bgm que fusiona por material, AO y luz), mallado de los nodos lejanos (`lod`: materiales, cortes por octante, faldones y paredes de agua, ADR 0031) | voxel |
| `boxel_light` | luz de vóxeles en CPU, sin ECS ni GPU (ADR 0015): techos de cielo por columna (`SkyTile`, caminata hacia abajo), luz exacta de un chunk desde una caja de 64³ (cielo y RGB, caminos rápidos para cuevas oscuras y cielo abierto), `PaddedLight`, `ChunkLight`; oráculo global para los tests | voxel |
| `boxel_worldgen` | trait `WorldGenerator` (con `lod()`, la fuente del terreno lejano: `LodSource`, ADR 0031), `FlatGenerator` y ruido entero exacto 2D y 3D (`hash3`, `fbm_3d`, `fbm_3d_grid`, `splitmix64`, `Frequency`) | voxel |
| `boxel_world` | `ChunkMap`, ciclo de vida, scheduler, jobs, ediciones, mensajes (`ChunkMeshEvent`, `BlockChanged`, `ChunkDataEvent`), guardar y cargar el mundo (`persist`: `ChunkStore`, `RedbStore`, `WorldStore`, `PersistPlugin`, ADR 0024), reloj del mundo (`TimeOfDay`, `DaylightCurve`, `DaylightPlugin`), ticks programados de bloques (`BlockTicks`, `BlockTicksPlugin`), agua y lava que corren y bloques con agua (`FluidPlugin`, ADR 0021 y 0025), las reglas de vecinos (`NeighborRules`, ADR 0020 y 0027), qué hace usar un bloque (`BlockUses`, una puerta que se abre, ADR 0027), la búsqueda de la ocultación de cuevas (`ChunkVisibility`, `visibility`, ADR 0022) y el lado del mundo del LOD lejano (`lod`: `LodWorldPlugin`, planificador, cobertura, trabajos, ADR 0031) | app, voxel, mesh, light, worldgen, transform, diagnostic |
| `boxel_render` | `VoxelRenderer` (High/Mid/Low), `RenderPlugin`, `SceneLight` (luz del cielo, ambiente y curva de brillo del frame), `PresentSettings` (vsync en tiempo de ejecución), texturas de las imágenes del registro o de un resource pack con las recetas que pasa el juego (`TextureSource`, `PackSource`), screenshots, ocultación de cuevas en los tres tiers (ADR 0022), contorno del bloque apuntado (`BlockOutline`, ADR 0026), texturas animadas de los packs (`LayerAnimation`, ADR 0028), conteo de draws en CPU (`cull_counts`), niebla y nubes cúbicas opcionales (`fog`: `FogPlugin`, `FogSettings`, `FogClock`, ADR 0030), terreno lejano opcional (`lod`: `LodPlugin`, `LodPass`, ADR 0031) | gfx, shaders, world, camera, window, pack, fog |
| `boxel_fog` | niebla y nubes cúbicas sin GPU ni ECS (ADR 0030): las capas (`FogLayer`, `validate`), el ruido entero que el shader repite bit a bit, cada capa en enteros chicos relativos al origen de render (`LayerParams`), la marcha de referencia de los tests y el empaquetado del texel | - |
| `boxel_physics` | física propia mínima (ADR 0016). Núcleo sin ECS: `CollisionTable` (desde `BlockDef::collision`: cubos y cajas que pueden subir media celda, como la valla de 1,5 bloques, más un contorno para apuntar, ADR 0018 y 0026), `VoxelSource`, barrido AABB por ejes contra la grilla (`move_box`), raycast DDA exacto en `i64` (`raycast_voxels`), rayos contra cajas y esferas, el integrador de un cuerpo (`step_body`, `predict`), el narrowphase (`shape_contact`) y la grilla del broadphase (`Grid`). Capa ECS: `ChunkSource` (lee `ChunkMap`), `PhysicsPlugin` (fuera de `DefaultPlugins`; pasos en `FixedPostUpdate`, interpolación en `AfterFixedMainLoop`), `PhysicsQuery` (rayos a vóxeles y cuerpos, cajas, el chequeo para poner bloques), `BodyIndex`, cuerpos (`RigidBody`, `Collider`, `Velocity`, `BodyPosition`, `Contacts`, `Interpolated`, `Teleport`, `Frozen`, `Sleeping`, `ReportContacts`), `CharacterController` (caminar, agacharse con el borde protegido y la caja de 1,5 (`Crouch`), correr con el salto corriendo y el vuelo creativo, ADR 0026), `Projectile`, nadar (`SwimPlugin`, `Swimmer`, ADR 0021; también en bloques con agua, y el vuelo no lo siente) y los mensajes `VoxelImpact`, `CollisionStarted`, `CollisionEnded` y `ProjectileHit`. Los cuerpos quietos se duermen (despiertan con `BlockChanged` y `ChunkDataEvent` del mundo, entre otras cosas) y, con muchos cuerpos despiertos, el paso corre en los hilos de cómputo. Fases P0 a P4 hechas | ecs, app, math, tasks, transform, voxel, world, diagnostic |
| `boxel_pack` | lector de resource packs de Minecraft Java (formatos ~64 a 120): `pack.mcmeta` con overlays, texturas de bloque PNG a RGBA8 (animaciones `.png.mcmeta`), modelos de bloque con herencia `parent` y variables `#`, blockstates `variants` y `multipart`. Solo datos; `boxel_render` lo usa para las texturas de bloque | (serde, png) |
| `boxel_demo` | contenido de los ejemplos, fuera del motor (ADR 0029, `publish = false`): bloques y capas (`blocks::registry`, `blocks::builder`), pintor procedural (`textures`), formas y reglas de escaleras, vallas, antorchas, muros y puertas (`shapes`, `rules`), agua y lava (`fluids`), recetas y tintes de packs (`pack`), generadores `TerrainGenerator`, `ForestGenerator` y `OverworldGenerator` con `Sea`, las cuevas y su fuente de LOD (`worldgen`, ADR 0031), presets de niebla y nubes (`fog`, ADR 0030) | voxel, worldgen, pack, fog |
| `boxel_facade_check` | solo CI: prueba que los derives resuelven `::boxel::ecs` para un usuario que depende solo de `boxel` | fachada |
| `vendor/*` (7 crates) | ECS y tareas (fork de Bevy) | - |

Features de la fachada: `default = [window, render, voxel, png, debug, physics]`, más `trace`
(spans de sistemas, implica `debug`), `trace_tracy` (Tracy), `track_location` y `hal` (escape
hatch Vulkan). `physics` implica `voxel` y expone `boxel::physics`. `debug` hace que los
diagnósticos del ECS (conflictos B0001, errores de schedule, spans) muestren nombres de sistemas
y componentes; conviene apagarla en builds de distribución.

---

## 5. Capa gráfica (`boxel_gfx`)

### 5.1 ¿Perdemos optimización con wgpu?

Respuesta corta: **en la GPU, no. En la CPU, solo si se dibuja de forma ingenua, y el diseño de
boxel lo evita.** La pérdida realista para un motor GPU-driven es de 0 a 5 % del tiempo de CPU del
frame y cerca de 0 % en GPU.

| Prueba | wgpu 30 | Vulkan crudo (ash 0.38) | Lectura |
|---|---|---|---|
| Compute DDA 1080p sobre grilla 256³, 10 dispatches, RTX 5050 | 55.0 a 55.5 ms | 54.6 a 57.1 ms | igual dentro del ruido (RADV también) |
| 10k draws con rebinding por chunk (bind group + VB + IB) | 8.5 a 37 ms CPU | 0.5 a 1.9 ms | 10 a 17x peor: patrón prohibido en boxel |
| `draw_indexed` pelado con `first_instance` = chunk | 0.58 ms | 0.15 ms | 2.6 a 4x |
| 1 `multi_draw_indexed_indirect` de 10k comandos | 36 µs | 16 µs | irrelevante en un frame de 16 ms |
| Igual con `VALIDATION_INDIRECT_CALL` activo | ~190 µs | - | por eso boxel lo apaga (§5.11) |
| Subida de 2 MiB por frame | 1.3 a 1.5x | 1x | menos de 0.5 ms en ambos |

Draws masivos en wgpu 30 (16 384 draws de 6 vértices, record+finish, NVIDIA Vulkan, sin validación
indirecta): direct 1.574 ms, direct + `set_immediates` 2.819 ms, `draw_indirect` xN 6.222 ms,
MDI 0.076 ms, MDIC 0.091 ms. Escena de 55 296 draws: con draws directos 3.1 a 3.3 ms de CPU; con
culling en compute y MDIC, 0.057 a 0.068 ms de CPU, y en RADV la GPU baja de 8.2 a 9.6 ms a 1.7 a
3.4 ms.

Conclusión: el sobrecosto de wgpu es **por comando**. Un motor que emite pocos comandos (un
multi-draw por lista) no lo nota. Por eso:

- Tier High: un `multi_draw_indexed_indirect_count` para los buckets opacos y otro para el
  cutout.
- Nunca se re-bindean buffers por chunk: un arena de quads, una tabla de chunks, un índice
  compartido. El dato por draw llega por un atributo de instancia (§10.4).
- Mid/Low sí hacen un draw por bucket visible (decenas a pocos miles por frame): 60 a 100 ns cada
  uno, menos de 0.5 ms con la distancia de vista por defecto.

Lo que wgpu **no** da y sí daría un backend propio: async compute (una sola cola), recursos
sparse, buffer device address, control fino de barreras, compilación asíncrona de pipelines,
pipeline cache fuera de Vulkan. Ninguno es necesario para el terreno. Si hace falta, el módulo
`boxel_gfx::hal` (feature `hal`) da acceso a `as_hal`/`open_with_callback`/SPIR-V passthrough en
Vulkan sin escribir un RHI. Costo estimado de un backend propio: 3 a 6 personas-mes por backend
más QA permanente de drivers (el backend Vulkan de wgpu-hal tiene 14.2k líneas, el de GLES 11.4k).

GL: en estado estable cuesta 1.25 a 1.5x la CPU de Vulkan por draw directo (88 ns contra 59 a
71 ns). El multi-draw indirecto en GL es un bucle en CPU y en el mismo GPU cuesta 4 a 12x más
tiempo de GPU que en Vulkan. Por eso GL nunca usa la ruta High.

### 5.2 Algoritmo de detección y selección (paso a paso)

Lo ejecuta `boxel_render` en el schedule `Render` (sistema `RenderSystems::Setup`, en el hilo
principal: `Render` usa el ejecutor de un solo hilo) cuando existe la ventana, y `gpu_info` en modo headless. Bloqueante (pollster); dura
50 ms a 1 s según si el dGPU estaba dormido (~1.05 s medidos al despertar la NVIDIA). Todo corre
en un solo hilo porque los error scopes de wgpu 30 son por hilo (`ErrorScopeGuard` no es `Send`).

1. **Preferencias.** `GpuPreferences::resolve(cli, config)`: CLI > `BOXEL_*` > `WGPU_*` > archivo
   de configuración > automático (§5.7). Los avisos van al reporte, nunca abortan.
2. **Instancia única.** `create_instance(Some(OwnedDisplayHandle), prefs)`:
   - backends = override o la política del SO (§5.10);
   - flags = política propia (§5.11), luego `InstanceFlags::with_env()`;
   - `Instance::new` dentro de `catch_unwind` con el hook de pánico silenciado (§5.12); si
     entra en pánico, reintento sin GL.
   El display handle es obligatorio para GL en Wayland (sin él EGL cae a surfaceless y la
   superficie falla con "incompatible window kind").
3. **Superficie nueva.** `instance.create_surface(Arc<Window>)` (`Surface<'static>`). Cada
   selección usa una superficie recién creada (§5.9, invariante).
4. **Enumeración.** `enumerate_adapters`; snapshot de capacidades (`AdapterCaps`: info,
   features, limits, downlevel, versión GL parseada de `driver_info`).
5. **Filtros** (cada descarte queda en el reporte): `Noop`/`BrowserWebGpu`; nombre (`--adapter`,
   substring sin mayúsculas; si no coincide nada es error explícito); backends no pedidos;
   `--software`; `is_surface_supported`; y **formato de superficie**: `choose_surface_format`
   elige Bgra8UnormSrgb, Rgba8UnormSrgb, Bgra8Unorm o Rgba8Unorm, nunca formatos float o de
   10 bits. Si no hay ninguno, el adaptador se descarta para esa superficie.
6. **Clasificación** (`classify`, §5.3): tier máximo que el adaptador declara, con razones.
7. **Candidatos.** Para cada adaptador, todos los tiers `t <= min(tier máximo, --tier)`. Puntaje
   (`score`, §5.4) por par (adaptador, tier). Orden estable descendente.
8. **Saltos.** Pares `Bad` en el LKG (salvo que el usuario los nombre con `--adapter` y `--tier`)
   y pares excluidos en esta sesión.
9. **Intento.** Para cada par en orden:
   - LKG: `begin_attempt` (marca `pending` con el pid, para detectar crashes);
   - `request_device` con los features y límites exactos del tier (`tier_request`);
   - instalar `on_uncaptured_error` (nunca entra en pánico; cuenta y registra) y
     `set_device_lost_callback` (marca `GpuHealth`);
   - smoke test del tier (§5.5) con el **formato exacto de la superficie**, dentro de scopes
     Validation + Internal + OutOfMemory; después, si el dispositivo se perdió o hubo errores no
     capturados, el par también falla;
   - si algo falla: soltar el dispositivo, `cancel_pending` en el LKG (un fallo dentro del
     proceso no se recuerda: se vuelve a detectar en milisegundos) y seguir.
10. **Ganador.** El primero que pasa. Se crea el `VoxelRenderer` (sus pipelines dentro de
    scopes) **antes** de configurar la superficie, luego se configura la superficie con el
    mismo formato que probó el smoke test. Si algo de esto falla, se excluye el par, se suelta
    todo, incluida la superficie, y en el siguiente frame se repite la selección con una
    superficie nueva. Se emite `GpuReady` y el reporte completo al log.
11. **Confirmación.** Tras el primer frame presentado, LKG `good` para el par.

Si ningún par sirve: `GpuFailed` con el reporte completo; la app decide (el ejemplo sale con
código 2).

### 5.3 Tiers y clasificación

Los tiers describen la **ruta de render** y se deducen de features, downlevel flags, límites y
versión GL, nunca solo del nombre del backend. Los umbrales numéricos son constantes compartidas
(`boxel_gfx::floors`) que usan tanto `classify` como `tier_request`, así la clasificación y el
pedido de dispositivo no pueden contradecirse:

| Tier | Requisitos (todos) |
|---|---|
| Low | backend real; `Features::IMMEDIATES`; `max_immediate_size >= 16`; `max_vertex_buffers >= 2` (el quad y su luz, ADR 0015); `max_texture_array_layers >= 256` |
| Mid | Low + `DownlevelFlags::VERTEX_STORAGE`; `max_storage_buffers_per_shader_stage >= 4`; `max_storage_buffer_binding_size >= 128 MiB`; en GL, versión parseada >= 4.3 (ES >= 3.1) |
| High | Mid + `COMPUTE_SHADERS | INDIRECT_EXECUTION`; `MULTI_DRAW_INDIRECT_COUNT | INDIRECT_FIRST_INSTANCE`; `max_compute_workgroup_size_x >= 64`; `max_compute_invocations_per_workgroup >= 64`; `max_compute_workgroups_per_dimension >= 1024`; backend Vulkan (DX12 solo con `TierPolicy::dx12_high`) |

`tier_request` parte de `Limits::default().or_worse_values_from(adapter)` (nunca mejor que el
adaptador en ningún campo) y sube a los valores del propio adaptador solo los límites que el motor
dimensiona desde el dispositivo (tamaño de buffers, capas de textura, immediates, storage). Un
test de contrato usa adaptadores sintéticos parados exactamente en cada umbral, y otro confirma
que los cuatro adaptadores reales de la máquina aceptan el pedido.

La clase **Software** (lavapipe, llvmpipe, WARP) queda limitada a Mid por defecto. Medido: en
lavapipe, High tarda 99 ms y Mid 5.5 ms para la misma imagen.

Por qué la versión GL: el contexto GL de NVIDIA en Linux es 3.3 (wgpu-hal pide 3.3 core y NVIDIA
devuelve exactamente eso), pero wgpu 30.0.1 reporta `COMPUTE_SHADERS` y `VERTEX_STORAGE` por las
extensiones (regresión del PR #9169). naga emite GLSL 330, que no tiene SSBO ni compute: la
creación del pipeline falla con error **Internal**. La clasificación lo evita y el smoke test lo
confirma.

Bits opcionales (no son tiers, se registran para milestones futuros): `TIMESTAMP_QUERY`,
`SUBGROUP`, `SHADER_INT64(_ATOMIC_MIN_MAX)`, binding arrays, `PIPELINE_CACHE`,
`POLYGON_MODE_LINE`, `EXPERIMENTAL_MESH_SHADER`/`RAY_QUERY` (solo opt-in, requieren
`ExperimentalFeatures::enabled()`).

### 5.4 Puntaje

| Componente | Valor |
|---|---|
| Tier | High 3000, Mid 2000, Low 1000 |
| Clase (HighPerformance, por defecto) | Discrete +400, Integrated +300 |
| Clase (LowPower) | Integrated +400, Discrete +300 |
| Clase (otras) | Other +200 (todos los adaptadores GL), Virtual +100, Software -5000 |
| Backend | Vulkan +100, Metal +100, DX12 +90, GL +0 |
| LKG | +50 si el par está `good`; -500 si está `suspect` (queda detrás de los otros adaptadores del mismo tier) |

El tier domina: un par High siempre se prueba antes que uno Mid. Software va siempre al final.
Empates: orden de enumeración.

En esta máquina, con la preferencia por defecto: NVIDIA Vulkan High (3500), RADV High (3400),
NVIDIA Vulkan Mid (2500), RADV Mid (2400), ..., NVIDIA GL Low headless (1200) y al final lavapipe
Mid (-2900). Con `--power low`: RADV High primero.

### 5.5 Smoke tests

El trait `SmokeTest` lo implementa el renderer (`RendererSmokeTest`): crea **sus pipelines
reales** del tier candidato (opaco, cutout y, en High, culling) y el pipeline de blit para el
**formato exacto de la superficie** que se va a usar (`SmokeContext::output_format`), graba un
pase y lo envía. Así, si un tier se elige es porque sus shaders verdaderos funcionan en ese
dispositivo con esa superficie. Para herramientas sin renderer (`gpu_info`) existe
`BasicSmokeTest`, con WGSL mínimo que ejercita las mismas clases de capacidad:

- Low: atributo de instancia `Uint32x2`, immediates, un uniform block, `texture_2d_array`, strip.
- Mid: Low + dos storage buffers de solo lectura en vertex + draw indexado.
- High: Mid + compute con `atomicAdd` sobre un buffer STORAGE|INDIRECT + un render pass que graba
  `multi_draw_indexed_indirect_count`.

Niveles: `Compile` (crea pipelines, graba un pase, `finish` y `submit`; wgpu 30 reporta los
errores de validación de pases en `finish`), `Draw` (además dibuja un quad fuera de pantalla y
lee un píxel) y `Auto` (por defecto): `Draw` en backends GL y en adaptadores Software, `Compile`
en el resto. Motivo: el bug de NVIDIA GL 3.3 (ADR 0011) no da ningún error, solo dibuja mal; una
lectura de 16x16 cuesta pocos milisegundos.

Los scopes son **Validation + Internal + OutOfMemory**. Internal es obligatorio: las fallas de
traducción de shaders en GL son Internal, no Validation; con solo Validation el handler por
defecto de wgpu haría pánico.

### 5.6 Fallbacks

En orden: siguiente tier del mismo adaptador (si el puntaje lo ubica antes), siguiente adaptador,
siguiente backend (todos están en la misma instancia), y por último Software. Ejemplos en esta
máquina:

- `--backend gl` sin display (headless): solo aparece NVIDIA GL 3.3. Mid se descarta por versión;
  Low pasa. Resultado: GL Low.
- `--backend gl` con ventana en Wayland: EGL usa la GPU del compositor, radeonsi 4.6. Resultado:
  GL Mid.
- Instancia con EGL de NVIDIA forzado en Wayland: `Instance::new` entra en pánico dentro de
  wgpu-hal; `catch_unwind` reintenta sin GL; si solo se pidió GL, error claro y código de
  salida 2.
- Build con `panic = "abort"` (común en juegos): `catch_unwind` no sirve, así que la instancia se
  crea primero **sin GL** y GL se intenta en una segunda instancia solo si la primera no encontró
  nada usable (§5.12).

### 5.7 Overrides y precedencia

Precedencia (de mayor a menor): **CLI > `BOXEL_*` > `WGPU_*` > archivo > automático**.

| Concepto | CLI | `BOXEL_*` | `WGPU_*` | archivo `[gpu]` |
|---|---|---|---|---|
| Backends | `--backend vulkan,gl` | `BOXEL_BACKEND` | `WGPU_BACKEND` | `backends` |
| Adaptador | `--adapter nvidia` | `BOXEL_ADAPTER` | `WGPU_ADAPTER_NAME` | `adapter` |
| Energía | `--power high/low` | `BOXEL_POWER` | `WGPU_POWER_PREF` | `power` |
| Tope de tier | `--tier high/mid/low` | `BOXEL_TIER` | - | `max_tier` |
| Solo software | `--software` | `BOXEL_SOFTWARE=1` | - | `software` |
| Validación | `--validation` | `BOXEL_GPU_VALIDATION=1` | `WGPU_VALIDATION` (flags) | `validation` |
| LKG | `--lkg` / `--no-lkg` | `BOXEL_LKG=1/0` | - | `use_lkg` |
| Present | `--present vsync/novsync/mailbox/immediate` | `BOXEL_PRESENT` | - | `present_mode` |
| Archivo | `--gpu-config ruta` | `BOXEL_GPU_CONFIG` | - | - |

Detalles: `WGPU_BACKEND` se parsea con `Backends::from_comma_list`; un resultado vacío se ignora
con aviso. `present` es el valor inicial de `PresentSettings` (vsync apagado si no se da, ver
§11): reemplaza el de la app una vez, al elegir la primera GPU, y después se puede cambiar en
tiempo de ejecución. El nombre de adaptador lo compara boxel; no se usa
`wgpu::util::initialize_adapter_from_env` porque hace pánico si no hay coincidencia (está
prohibido en `clippy.toml`). `--tier` es un tope, no una orden.

### 5.8 Memoria de crashes por dispositivo (LKG)

El LKG existe para una sola cosa: **sobrevivir a crashes que no se pueden atrapar dentro del
proceso** (un segfault del driver durante la creación del dispositivo, un cuelgue que el usuario
mata). Los fallos que sí se ven dentro del proceso (`request_device` falla, el smoke test falla)
no se guardan: se vuelven a detectar en cada arranque en milisegundos.

Archivo TOML en `$XDG_CONFIG_HOME/boxel/<app>/gpu-lkg.toml` (Windows `%APPDATA%`, macOS
`~/Library/Application Support`), escritura atómica, versión 2. Cada entrada: adaptador (nombre,
backend, clase, vendor, device, driver, `driver_info`), tier, estado (`pending`, `good`,
`suspect`, `bad`), `strikes`, versiones de boxel y wgpu, **huella del build** (versión + fecha de
modificación del ejecutable, sin build script), pid, estado previo, fecha y motivo.

- Antes de cada intento: `pending` con el pid y el estado previo.
- Tras el primer frame presentado: `good`, `strikes = 0`.
- Salida limpia (schedule `Shutdown`), pánico de Rust (hook instalado por el renderer) o fallo
  dentro del proceso: se restaura el estado previo. Un pánico de Rust no es un crash del driver.
- Al abrir el archivo: un `pending` de otro pid con más de 30 s (menos puede ser otra instancia
  arrancando en paralelo) suma un strike: el primero deja el par en `suspect` (se sigue probando,
  con -500 de puntaje), el segundo en `bad` (se salta).
- Una segunda pérdida de dispositivo en la misma sesión también suma un strike (no si la causó
  `DebugDestroyDevice`).
- Una entrada solo aplica si coinciden adaptador, `driver_info`, versiones y build: actualizar el
  driver o recompilar le da otra oportunidad al par.
- Con flags de depuración activos (`--validation`, `WGPU_VALIDATION...`) el LKG es de solo
  lectura: esas configuraciones fallan por motivos que no dicen nada del driver.
- Por defecto está activo en builds release y apagado en builds con `debug_assertions` (en
  desarrollo los pánicos son normales). `--lkg` lo fuerza; `gpu_info --lkg <app>` lo muestra y
  `--clear-lkg <app>` lo borra.

### 5.9 Superficie, dispositivo perdido y re-selección

Invariante: **una `Surface` que se configuró alguna vez nunca se configura con otro
dispositivo**. En Wayland, reconfigurar la misma superficie con un dispositivo nuevo produjo
segfault (medido dos veces). Cada selección usa una superficie nueva creada desde el mismo
`Arc<Window>`, y la vieja se suelta antes.

`set_device_lost_callback` marca `GpuHealth` (atómico, desde cualquier hilo). El sistema `Draw` no
adquiere nada si el dispositivo está perdido. En el siguiente `Setup`:

1. Se registra el motivo y se emite al log.
2. Se sueltan, en este orden: renderer, superficie, dispositivo y cola. La instancia se conserva.
3. Política: la primera pérdida reintenta el mismo par (TDR transitorio). Una segunda pérdida en
   la sesión excluye el par (`Exclusion`) y suma un strike en el LKG (salvo si la causó
   `DebugDestroyDevice`); la selección baja de tier o cambia de adaptador.
4. El renderer se reconstruye desde el estado del ECS: `ChunkMap::ready_meshes()` conserva todas
   las mallas en CPU, así que cambiar de tier o de dispositivo es una recarga, no un reinicio.

Si `get_current_texture` devuelve `Lost` con el dispositivo sano, se suelta primero la superficie
vieja y recién después se crea otra con el mismo dispositivo (Vulkan permite una sola swapchain
viva por ventana).

Para verificarlo existe el mensaje `DebugDestroyDevice` (`voxel_world --simulate-device-loss N`),
que llama `device.destroy()` (verificado en todos los adaptadores de la máquina: llega el callback
con `DeviceLostReason::Destroyed` y se puede crear otro dispositivo en el mismo adaptador). Esa
pérdida nunca suma strike en el LKG: una prueba no cambia la GPU de la próxima ejecución. La
recuperación cambiando de backend sobre la misma ventana no se pudo ejercitar (riesgo abierto).
Si el presupuesto del M0 se agota, la re-selección en caliente es lo primero que se recorta: queda
el aviso, `GpuFailed` y salida limpia con código 2.

### 5.10 Política de backends por sistema operativo

| SO | Backends compilados | Instancia por defecto | Notas |
|---|---|---|---|
| Linux/BSD | vulkan, gles | `VULKAN | GL` | GL sigue al EGL del display: en Wayland/XWayland la GPU del compositor. Hybrid: `--power` elige NVIDIA o AMD en Vulkan; GL no puede elegir la dGPU sin variables PRIME |
| Windows | vulkan, dx12, gles | `VULKAN | DX12` | GL solo con `--backend gl`. DX12 limitado a Mid hasta verificar High en hardware real (para experimentar: `BOXEL_DX12_HIGH=1` en `gpu_info`; en apps, `RenderSettings::tier_policy`). Compilador de shaders: `Dx12Compiler::Auto` usa `dxcompiler.dll` si está en el PATH y si no FXC (lento, sin mantenimiento); la CI de Windows mide el tiempo del smoke test y M1 decide entre `static-dxc` y distribuir la DLL |
| macOS | metal | `METAL` | Metal no tiene MDI-count en wgpu: tope Mid. El módulo `hal` no existe en Apple (wgpu-hal no compila Vulkan ahí sin `vulkan-portability`) |

### 5.11 Política de `VALIDATION_INDIRECT_CALL` y límites del multi-draw

wgpu 30 activa este flag por defecto incluso en release (`from_build_config` devuelve
`VALIDATION_INDIRECT_CALL` aunque su documentación diga `empty()`). boxel **no lo activa nunca**
(se puede forzar con `WGPU_VALIDATION_INDIRECT_CALL=1` para depurar). Razones:

1. Rompe GL en NVIDIA: con el flag, `request_device` falla con "Parent device is lost" porque el
   pase de validación necesita compute y el contexto es 3.3.
2. Es por instancia, no por backend.
3. boxel no depende de lo que protege: nunca lee `instance_index`, `draw_index` ni
   `num_workgroups`, siempre usa `first_vertex = 0` y `base_vertex = 0` (§10.4), y su única ruta
   indirecta es la variante `*_count`, que wgpu no valida de todos modos.
4. Cuesta CPU: MDI de 16k: 0.547 ms con el flag contra 0.076 ms sin él.

Contrapartida: el shader de culling debe generar argumentos válidos. Lo garantiza así:
`first_index = 0`, `base_vertex = 0`, `instance_count = 1`, `index_count` recortado con `min` a
`6 * 98304` (lo que cubre el índice compartido; ver §9), `first_instance` dentro de la lista, nunca
escribe más allá de la capacidad de cada lista y el contador guardado nunca la pasa (un bucket que
encuentra la lista llena deshace su `atomicAdd`).

Límite adicional (verificado en wgpu-hal 30.0.1): en la variante `*_count`, wgpu-hal pasa
`max_count` a `vkCmdDrawIndexedIndirectCount` sin compararlo con `maxDrawIndirectCount`, cuyo
mínimo en Vulkan es 65535 (los drivers de escritorio reportan 2³² - 1). boxel limita cada lista,
cada llamada y cada contador a 65535 draws. Si hay más buckets visibles (no pasa con la
configuración por defecto), High omite algunos sin orden (no los más lejanos: pueden faltar chunks
cercanos), Mid y Low dibujan todos, y se avisa una vez. M3 segmenta las listas. Ese mínimo vale
solo si el dispositivo tiene `multiDrawIndirect` (si no, el límite es 1), y wgpu ofrece el
multi-draw con count aunque no lo tenga (riesgo abierto; las GPUs de escritorio lo tienen).

### 5.12 `catch_unwind` en la creación de la instancia

`wgpu::Instance::new` puede hacer pánico en lugar de devolver error (medido: con
`__EGL_VENDOR_LIBRARY_FILENAMES` apuntando a NVIDIA en Wayland, khronos-egl hace `unwrap` dentro de
`wgpu_hal::gles::egl::Instance::init`). `create_instance` envuelve la llamada en
`std::panic::catch_unwind` (con `AssertUnwindSafe`) y reemplaza el hook de pánico durante la
llamada por uno silencioso (un pánico manejado no debe imprimir un backtrace alarmante; el mensaje
va a las notas del reporte). Ante pánico reintenta sin `GL`. Si no queda ningún backend,
`SelectError::NoInstance` con el texto del pánico.

Con `panic = "abort"` no hay unwinding: la instancia se crea sin GL cuando se pidieron otros
backends (`gl_deferred`), y si esa instancia no encuentra nada usable se crea una segunda solo con
GL (con una superficie nueva). Si el usuario pidió solo GL, se crea directamente.

### 5.13 Escape hatch `hal`

Feature `hal` de `boxel_gfx`: módulo `hal` con funciones `unsafe` sobre `wgpu::hal` (misma versión
30.0.1, ash 0.38). Solo existe en Windows, Linux, Android y FreeBSD: la feature `vulkan` de wgpu
llega a wgpu-hal solo en esas plataformas (verificado en el `build.rs` de wgpu y en el manifiesto de
wgpu-core). M0 incluye solo `with_vulkan_device`. Cualquier uso nuevo requiere ADR.

---

## 6. ECS y capa de aplicación

### 6.1 Estrategia: fork vendorizado de `bevy_ecs`

Decisión (ADR 0003): `vendor/` contiene un fork **recortado y renombrado** de `bevy_ecs`
0.20.0 y sus dependencias internas (`boxel_ecs`, `boxel_ecs_macros`,
`boxel_ecs_macro_logic`, `boxel_macro_utils`, `boxel_ptr`, `boxel_tasks`, `boxel_utils`).
`bevy_platform` se reemplaza por un shim de unas 80 líneas propias más 3 archivos vendorizados.
Se eliminan `bevy_reflect`, hotpatching y stepping.

Reglas:

1. **Diff de librería cero.** El fork se genera con `tools/refork/refork.sh` (fuentes de
   crates.io ubicadas con `cargo metadata`, renombres mecánicos con verificación, manifiestos
   propios, parches solo de tests, backports). En cada regeneración `check_test_only.py` compara
   el árbol solo-renombrado con el parchado y falla si algún cambio cae fuera de código de test o
   de documentación. Cada archivo `.rs` modificado lleva una línea "boxel: modified by
   tools/refork" (Apache-2.0 §4(b)). Verificado sobre 0.20.0: 962/962 tests de librería de
   `boxel_ecs` y 480/480 doctests pasan con `--locked`.
2. `vendor/` es **su propio workspace de Cargo** (excluido del raíz): conserva los lints de Bevy y
   tiene su `Cargo.lock` versionado, que el script conserva entre regeneraciones.
   `tools/vendor_lock_sync.py` verifica que coincida con el lockfile raíz en los paquetes
   compartidos (si no, los tests del fork no cubren lo que se distribuye).
3. Todo lo propio de boxel vive **fuera** de `vendor/` (App, plugins, componentes, SystemParams).
4. El código del motor y los ejemplos usan una superficie reducida de `boxel_ecs` (módulos
   `prelude`, `component`, `entity`, `message`, `query`, `resource`, `schedule`, `system`,
   `world`, `change_detection`, `lifecycle`, `intern`, `label`, `error`, `bundle`), controlada por
   `tools/ecs_surface.py` (detecta también imports agrupados y rutas `boxel::ecs::`). Alcance
   honesto de la "salida de emergencia": cubre el código del motor. La fachada re-exporta
   `boxel_ecs` completo y los derives de los usuarios expanden a rutas internas (system, query,
   world...); reemplazar el núcleo rompería el código de usuarios que use derives o items fuera
   del prelude.
5. Re-fork sobre 0.20.0 final: hecho el 2026-10-09 (issue #24, detalle en ADR 0003). 0.20.0 no
   trae bevy#25940, así que el backport sigue; el script falla pidiendo borrarlo cuando un parche
   ya está upstream. Próximo re-fork como máximo en un año o por un fix de seguridad; entre
   re-forks se aplican como parches los PR de Bevy etiquetados unsound/UB.
6. Miri en un workflow propio: semanal y en cada PR que toca `vendor/**` o `tools/refork/**`
   (`boxel_ecs`, `boxel_ptr`, `boxel_tasks`, nightly fijado, `-Zrandomize-layout`).
7. Licencias: cada crate vendorizado conserva LICENSE-MIT y LICENSE-APACHE; `NOTICE` en la raíz
   atribuye a los contribuidores de Bevy y no usa marcas de Bevy.

Gotcha documentado: rustc tiene un parche de compatibilidad que solo funciona para un tipo
`ParamSet` dentro de un crate llamado `bevy_ecs`. Tras el renombre, `ParamSet<(Query<&mut A>,
Query<&B>)>` con lifetimes elididos falla con E0477 ("the type `&mut A` does not fulfill the
required lifetime"): hay que escribir `&'static`.

Macros derive: resuelven la ruta del ECS leyendo el `Cargo.toml` del usuario. Si depende de
`boxel_ecs` usan `::boxel_ecs`; si depende de `boxel` usan `::boxel::ecs`. El crate
`boxel_facade_check` (solo CI) depende únicamente de la fachada y prueba todos los derives por esa
ruta, que es la que ven los usuarios reales.

Nota de bevy_ecs 0.20: los recursos son entidades (aparecen en los conteos de entidades).

### 6.2 `boxel_app`

Propio, ~700 líneas, inspirado en `bevy_app` 0.20:

- `App`: un `World`, registro de plugins, runner `Box<dyn FnOnce(App) -> AppExit>`.
  `App::new()` instala `MainSchedulePlugin`, la rotación de mensajes en `First` y el mensaje
  `AppExit`. Métodos estilo Bevy: `add_plugins`, `add_systems`, `configure_sets`, `add_message`,
  `insert_resource`, `init_resource`, `insert_non_send`, `init_non_send`, `add_schedule`,
  `edit_schedule`, `update`, `run`, `should_exit` (el primer error si lo hay, si no `Success`),
  `plugins_state`, `finish` y `cleanup` (un plugin agregado durante el `finish` de otro queda
  registrado y también se finaliza), `shutdown`.
- `Plugin` (`build`, `ready`, `finish`, `cleanup`, `name`, `is_unique`); también funciones
  `Fn(&mut App)`. `Plugins<Marker>` acepta plugin, grupo o tuplas. `PluginGroup` +
  `PluginGroupBuilder` (`add`, `set`, `disable`, `enable`, `add_before`, `add_after`).
- Schedules: `Main` corre una vez `PreStartup`, `Startup`, `PostStartup` y cada frame `First`,
  `PreUpdate`, `RunFixedMainLoop`, `Update`, `PostUpdate`, `Last`, más lo insertado con
  `MainScheduleOrder::insert_after` (el renderer agrega `Render` después de `Last`). `Main`,
  `FixedMain` y `Render` usan el ejecutor single-thread (`Render` lo configura el renderer con
  `edit_schedule`); el resto, el multi-thread del fork. `Shutdown` no es parte del frame: los
  runners lo corren una vez al salir (`App::shutdown`), con la ventana todavía viva, para soltar
  recursos de GPU y ventana en orden. `Idle` tampoco (ADR 0023, un hilo): con límite, los runners
  lo corren en pasos cortos mientras esperan el frame siguiente (`App::run_idle`, `IdleStep`,
  `IdleWork`), hasta 1 ms antes del deadline; el mundo recoge resultados y lanza jobs ahí. Si la
  app tiene `IdleWanted`, solo después de una update que pidió pasos (el mundo los pide cuando le
  queda trabajo).
- Tiempo: `Time` (delta con tope de 250 ms, elapsed, frames) en `First`; `FixedTime` a 64 Hz con
  acumulador y máximo de 8 pasos por frame (`set_max_steps_per_frame` lo cambia); `FixedMain` =
  `FixedPreUpdate`, `FixedUpdate`, `FixedPostUpdate`. Mientras corre `FixedMain`, `Res<Time>` es
  la vista fija (delta = paso fijo), como en Bevy. `RunFixedMainLoop` se divide en los sets
  encadenados `RunFixedMainLoopSystems` (los nombres de Bevy): `BeforeFixedMainLoop` (muestrear
  la entrada para los pasos), `FixedMainLoop` (los pasos) y `AfterFixedMainLoop` (interpolar).
  `PhysicsPlugin` baja el tope a 3 pasos (para toda la app: por debajo de 21.3 fps el juego
  de paso fijo va en cámara lenta; `voxel_world` deja los 8) y corre `FixedPostUpdate` y
  `RunFixedMainLoop` con el ejecutor de un hilo (sus sistemas van encadenados; el multi-hilo
  solo sumaba unos 24 µs por corrida).
- Mensajes y paso fijo: `TimePlugin` agrega `signal_message_update_system` en `FixedPostUpdate`
  y deja la rotación de mensajes en espera hasta que corra un paso fijo. Sin esto, un lector en
  `FixedUpdate` perdía los mensajes escritos en `Update` cuando los frames iban a más del doble
  de la frecuencia fija (medido: 20 de 30 perdidos).
- Task pools: `TaskPoolPlugin` crea los tres pools globales de `boxel_tasks`. El hilo principal
  también ejecuta sistemas, así que los workers son `paralelismo - 1`: IO 10 % (1 a 2), async
  compute 40 % (1 a 8, jobs de chunks), compute el resto. Con 16 hilos: 2, 6 y 7. La física usa
  compute para su paso en paralelo (`par_iter_mut` desde 256 cuerpos despiertos, ADR 0016).
- Runners: `ScheduleRunnerPlugin` (headless, `Once` o `Loop`) y el runner winit de
  `boxel_window`. El runner por defecto actualiza una vez. Los dos respetan `FrameLimit` (144 fps
  por defecto, 0 = sin límite, cambiable en tiempo de ejecución) con el mismo `FramePacer`
  (§11, ADR 0013); en headless, `FrameLimit` reemplaza el `wait` de `Loop` cuando existe. Los
  dos corren los pasos de `Idle` en la espera y publican `IdleInfo` (ADR 0023).

---

## 7. Modelo de datos de vóxeles

- **Chunk de 32³** (`CHUNK_SHIFT = 5`). Potencia de dos: coordenadas con shifts y máscaras, LOD
  por mips 2x, edición barata (~0.1 ms de remesh contra ~0.56 ms con 62³). El mesher usa
  `Mesher<32>` con borde (34 = 32 + 2, entra en una columna `u64`).
- **Índice local** `(y << 10) | (x << 5) | z` (z más rápido): mismo orden que el `pad_linearize`
  de binary-greedy-meshing, así el gather copia filas.
- **Coordenadas:** `ChunkPos` y `BlockPos` en `i64`, `LocalPos` (u16), `ChunkKey { dim, lod,
  pos }`, `Face` (orden congelado `+Y, -Y, +X, -X, +Z, -Z`), 26 vecinos en orden fijo. Límite
  de mundo único: `MAX_CHUNK_COORD` = ±2^56 chunks (±2^61 vóxeles). Dentro de él ningún origen,
  vecino o radio desborda `i64`, y las distancias al cuadrado van en `u128`. El mundo sujeta a
  los viewers a ese límite. Un chunk de lod k empieza en `pos << (5 + k)`. Detalles en
  ADR 0014.
- **Registry** de datos: `BlockState(u16)` denso en tiempo de ejecución (0 = aire), nunca se
  persiste crudo (M1 agrega el IdMap, que guarda el texto del estado). Un bloque tiene
  propiedades con valores finitos, como en Minecraft (ADR 0019): cada combinación es un estado,
  con ids seguidos por bloque, y se escribe `boxel:stone_slab[type=top]` (`parse_state`,
  `state_name`, `property`, `with`). Como mucho `MAX_STATES` = 2^15 estados, así el bit alto del
  material del mesher queda libre. Tablas SoA por estado: nombre, capa de dibujo (`None`,
  `Opaque`, `Cutout`, `Translucent`), modelo (`Cube { axis }`, `Slab`, `Cross`, ADR 0018;
  `Stairs`, `Fence`, `Torch`, `WallTorch`, ADR 0020, hechos de cajas en dieciseisavos),
  colisión (`None`, `Full` o cajas), flags, emisión RGB, opacidad de luz (15 en los cubos opacos,
  1 en hojas, agua y losas, 0 en vidrio y plantas), texturas por cara. Solo un cubo `Opaque`
  tapa caras vecinas (`BlockRegistry::occludes`). Contenido congelado: aire,
  piedra, tierra, pasto, arena, grava, nieve, bedrock, tronco, hojas (cutout), agua (translúcida,
  `boxel:water[level=0..15]` desde ADR 0021),
  tablones y, desde la fase P6 de la luz, los primeros emisores: lámparas de cubo completo naranja
  (14, 11, 6), roja (14, 2, 2), azul (3, 6, 14) y blanca (15, 15, 15) y una lava provisoria
  (15, 7, 2), opaca y sólida hasta que haya fluidos (`boxel_voxel::LAMPS`, `LAVA_EMISSION`);
  desde ADR 0018, vidrio, `boxel:stone_slab[type=bottom|top|double]` (la doble es un cubo de
  piedra), pasto corto, amapola y `boxel:log[axis=x|y|z]`; desde ADR 0020, escaleras de piedra y
  de roble, la valla de roble y las antorchas (de pie y de pared, emisión 14). La forma de una
  escalera, los lados de una valla y el soporte de una antorcha siguen a sus vecinos
  (`boxel_voxel::rules`, aplicadas por `boxel_world::NeighborRules` en cada lote de ediciones).
  Desde ADR 0025, la lava corre (`boxel:lava[level=0..15]`), hay adoquín y obsidiana, y losas,
  escaleras y vallas guardan agua (`waterlogged`).
- **Contenedor de paleta:** `Uniform(state)` | `Packed { palette, bits 1/2/4/8, words }` |
  `Direct(u16)`. Los índices nunca cruzan palabras de 64 bits. `set` agrega a la paleta en el
  lugar mientras entra; se compacta al guardar o en reposo. Medido en un mundo de 512x256x512:
  60.6 % de los chunks son aire uniforme; la paleta reduce 128 MiB a 9 MiB (0.14 B/vóxel).
- **Luz:** no vive en el chunk. Es un dato derivado, función pura de los bloques y de un techo de
  cielo por columna, que calcula `boxel_light` (ADR 0015) dentro del job de malla; el mesher la
  hornea por esquina y el fragment shader la interpola (sección 10.7). Los chunks cerca del
  viewer guardan su luz para gameplay (`ChunkMap::light_at`, que también muestra el overlay F3):
  la del job de malla o, si ese job no la guardó, la de un job de luz (sección 8).
- **Máscaras de luz** de 4 KiB (1 bit por vóxel): `LightMasks` (opaca, parcial y emisores). Los
  chunks uniformes y los de 1 bit sin emisores la sirven desde sus propias palabras; los demás la
  construyen la primera vez que se pide y la guardan en caché hasta que cambian sus bloques.
  `ChunkData::from_occupancy` hace el camino inverso: arma un chunk de 1 bit (o `Uniform` si la
  máscara está toda llena o toda vacía) directo de la máscara, sin arreglo denso. El overworld lo
  usa para los chunks de piedra y cuevas bajo la superficie.
- **`ChunkData`** = bloques + `version` + caché de máscaras de luz. Se comparte como `Arc<ChunkData>`; las ediciones
  usan `Arc::make_mut`. Los arrays de vóxeles nunca van inline en componentes del ECS.

---

## 8. Ciclo de vida de chunks y scheduling de jobs

Los chunks **no son entidades**. `ChunkMap` (recurso) = `SlotMap<ChunkId, ChunkSlot>` +
`HashMap<ChunkPos, ChunkId>` con foldhash `FixedState` (determinista; 6.5 ns por lookup medidos).
El mapa lleva al día los contadores de `WorldStats` en cada cambio de un slot (`data`, `mesh` y
`remesh_pending` solo cambian por sus métodos), así que leerlos no recorre los slots.
Los viewers sí son entidades (`Transform` + `ChunkViewer { radius, vertical_radius }`).

Estados ortogonales por slot:

- `DataState`: `Queued`, `Generating`, `Ready(Arc<ChunkData>)`, `Missing` (generación
  fallida: aire permanente).
- `MeshState`: `None`, `Queued`, `InFlight { data_version, epoch }`, `Ready { mesh, data_version,
  epoch }`, `Empty`.
- `Interest`: `DATA`, `MESH`, `PINNED`.
- `neighbors_ready`: máscara de 26 bits, válida desde que llegan los datos del propio slot (antes
  nadie la lee, y crear un slot no busca a sus vecinos); `epoch`: contador que sube cuando cambia
  el borde de un vecino o su luz; `cancel`: `Arc<AtomicBool>` para cancelación cooperativa;
  `light`: la luz del chunk (`ChunkLight`) dentro de `light_cache_radius`, con la versión de
  datos y el `epoch` con que se calculó (solo vale si siguen iguales).

La caché de luz sigue al viewer: un chunk con malla dentro de `light_cache_radius` (8) cuyo job de
malla no guardó la luz (todo aire, encerrado, un job fallido, o que entró al radio después de
mallarse) recibe un job de luz (`JobKind::Light`), que calcula solo la luz y va detrás de las
mallas de la vista. Un chunk uniforme de un bloque opaco sin emisión queda oscuro sin job. Cuando
un viewer cambia de chunk se recorren solo las cáscaras que entran y salen del radio: las que
entran piden su luz, las que salen la sueltan.

`SkyMap` (recurso) guarda un `SkyColumn` por columna de chunks con algún slot: el `SkyTile` con
el techo de cielo de cada una de sus 32x32 columnas, calculado por un job `SkyTile` que camina la
columna hacia abajo (ADR 0015). Se crea con el primer slot de la columna y se borra con el último,
y lleva las capas de sus slots cargados, ordenadas: caminatas, bandas y búsquedas de techo
recorren solo esas capas, nunca un rango numérico (dos viewers a 2^35 capas en la misma columna
no cuestan más que dos vecinos). Una columna se camina solo cuando una malla la necesita, y antes
espera los datos cargados que va a leer.

Cada viewer quiere un cilindro vertical: malla en el círculo `radius` (xz) y en las capas a
`vertical_radius` o menos (8 por defecto); datos un chunk más en cada dirección (círculo
`radius + 1` más los vecinos xz de cada columna con malla, que el círculo solo deja fuera en
algunas diagonales del borde); descarga más allá de otros `hysteresis` (2) chunks, en xz y en y.
El mundo no tiene alto máximo; `WorldSettings::vertical_bounds` fija uno si hace falta (con el
rango del generador y un `vertical_radius` grande reproduce el streaming de M0). Las capas fuera
de `WorldGenerator::vertical_chunks()` se crean como aire sin job: el rango es una promesa del
generador ("afuera todo es aire"), no un límite del mundo. El overworld no tiene piso: su rango
empieza en `i64::MIN`, y el mundo carga hacia abajo hasta el radio vertical del viewer. Un vecino sin cargar no cuenta como
listo, salvo fuera de `vertical_bounds`.

Sistemas en `Update`, encadenados (`WorldSystems`):

1. **Interest:** cuando un viewer cambia de chunk o de radio, lista solo las posiciones cuyo
   nivel (malla, datos, banda de descarga, fuera) cambia: un paso corto se compara fila por fila,
   O(filas + cambios); un viewer nuevo o un salto lista su región nueva por cáscaras, la más
   cercana primero, y después la vieja. Cada posición listada se evalúa contra los viewers
   actuales dentro de `interest_budget` (1 ms por frame), en tandas de 32: primero se buscan todas
   en el mapa (así sus fallos de caché se solapan) y después se evalúan. Crea slots (prioridad =
   distancia² al viewer más cercano), actualiza el interés o descarga (marca `cancel`, emite
   `ChunkMeshEvent::Remove`, invalida vecinos). Las prioridades de la cola quedan viejas y se
   recalculan por lotes, las más cercanas primero. El mapa reserva lugar para dos vistas: al
   saltar, la vieja sigue cargada hasta que se crea la nueva y el mapa no crece a mitad de la
   carga.
2. **Edits:** aplica `EditQueue` agrupando por chunk: `make_mut`, `set`, sube `version`, y
   remalla como un lote (prioridad 0, sube el `epoch`): todo chunk cuya región acolchada contiene
   un vóxel cambiado (caras, AO y luz de esquina: hasta 8); si cambió la opacidad o la emisión,
   todo chunk al alcance del cambio de luz (a lo sumo 14 vóxeles, medido con la luz guardada); si
   se movió el techo de cielo de la columna, los chunks de la banda alrededor de la columna. Un
   techo que no se resuelve con lo cargado manda la columna a caminar de nuevo. Escribe un
   `BlockChanged { pos, old, new }` por cada bloque que cambió en el frame, sacado de la misma
   lista de cambios por vóxel que usa la luz (dos ediciones del mismo bloque dan un mensaje, del
   estado de antes al de después; las que no cambian nada o se descartan no dan ninguno), en el
   orden en que se editó cada posición por primera vez.
3. **Dispatch:** primero vacía el canal de resultados (libera sus lugares en vuelo) y después
   saca jobs mientras `in_flight < tope`; repite mientras lleguen resultados, dentro de
   `job_budget` (2 ms por frame). Antes de sacar un job recalcula las prioridades viejas que
   podrían ir antes que él: el viewer se acercó como mucho tanto como se movió, y eso acota cada
   prioridad vieja. Así los jobs salen siempre del más cercano al más lejano. El tope por defecto
   sale de los hilos de async compute, del costo medido de un job (promedio de cada frame,
   suavizado entre frames) y del intervalo entre frames: trabajo para 1.5 frames (al menos 2 por
   hilo, como mucho 4096). Generación: captura generador, registry y clave. Malla: solo si los 26
   vecinos están `Ready` o `Missing` y, si su caja de luz puede tener cielo directo, los techos
   de sus 3x3 columnas están calculados; captura el centro y los 26 `Arc`, los 9 tiles, la tabla
   de materiales, `data_version` y `epoch`, y en el hilo calcula la luz y después malla. Atajos:
   centro aire uniforme, o centro opaco uniforme rodeado de opacos uniformes en las 6 caras, da
   `Empty` sin job. Descarta resultados viejos (generación del slotmap, `data_version`, `epoch`,
   `cancel`; en las caminatas, la `version` de la columna).
4. **Collect:** publica `ChunkMeshEvent::Upsert` / `Remove` de las mallas listas, hasta 512 por
   frame. Una malla igual a la publicada no se envía. Las mallas de un lote de ediciones salen
   juntas y antes que las del streaming, fuera de ese tope: una retenida en el frame N espera al
   resto hasta el frame N + 2 y sale ahí como mucho. Los jobs del lote mantienen prioridad 0
   aunque el viewer se mueva. Si otra edición toca un chunk con su malla retenida, esa malla se
   descarta y el chunk se remalla enseguida, en el lote unido. Después publica los
   `ChunkDataEvent` del frame, en el orden en que pasaron: `Loaded { id, key }` cuando los
   bloques de un chunk se pueden leer (`Ready`, o `Missing`, que se lee como aire) y
   `Unloaded { id, key }` cuando dejan de poder leerse. El mapa los anota al cambiar el estado de
   un slot (`set_data` y `remove`), así ningún camino del streaming se olvida de avisar.
5. **Visibility:** pasa a `ChunkVisibility` los cambios del frame en los enlaces de caras de los
   chunks (ADR 0022). Un chunk aporta sus enlaces (calculados por su job de generación, o por su
   job de malla después de una edición) solo si el render lo muestra como es: su malla publicada
   se hizo con sus bloques y los bordes de sus vecinos de ahora. Un chunk con bloques que no se
   muestra así se ve a través (la búsqueda ve todo lo que hay detrás); uno sin datos cuenta como
   abierto. La búsqueda la pide el render en su `Extract`, con su cámara.

Con `BlockTicksPlugin` (lo agrega `FluidPlugin`, ADR 0021), entre Interest y Edits corren los
ticks programados que vencieron, en orden y con un presupuesto de 1 ms. Sus escrituras entran al
`EditQueue` del mismo frame. Después de Edits, los `BlockChanged` de las demás ediciones programan
los bloques con tick que están en la posición o al lado. El reloj de ticks (20 por segundo) avanza
en `FixedUpdate`.

`BlockChanged` y `ChunkDataEvent` son los mensajes que comparten los sistemas que guardan algo
derivado de los bloques: la física despierta los cuerpos dormidos cerca de un bloque que cambió
o de un chunk que se cargó (ADR 0016, P4). La luz del mundo no los necesita: la recalcula el
lote de remallas que arma la misma pasada de ediciones. En el frame en que se escribe un
`BlockChanged` (después de `WorldSystems::Edits`) el mapa ya tiene el bloque nuevo, pero la luz
guardada (`ChunkMap::light_at`) y las mallas pueden seguir siendo las de antes: cambian cuando
llegan las remallas del lote (en general unos frames después). Un sistema en un schedule fijo
(como la física) recibe todos los mensajes, pero quizá frames después, cuando otra edición ya
cambió el bloque o su chunk se descargó: lee el mapa en vez de confiar en `new`.
Un chunk que se descarga y se vuelve a cargar puede volver distinto (todavía no se guardan las
ediciones), por eso quien guarda algo de un chunk lo refresca en `Loaded`.

**Contrato de resultados:** todo job enviado devuelve **exactamente un** mensaje: `Generated`,
`Meshed`, `Walked` (una caminata de cielo), `Cancelled` (vio la bandera de cancelación) o
`Failed` (con el texto del pánico). El
cuerpo del job corre dentro de `catch_unwind` y el job lleva un token cuyo `Drop` envía `Failed`
si no se envió nada. Motivo: los hilos de `boxel_tasks` atrapan el pánico de una tarea y siguen;
sin el contrato, cada job perdido dejaba ocupado un lugar "en vuelo" y tras 16 pérdidas el
streaming se detenía para siempre. El pool se obtiene con `get_or_init`, así el mundo funciona
aunque falte `TaskPoolPlugin`.

El chequeo por `epoch` cierra el hueco detectado en la investigación: un remesh construido con
snapshots viejos de un vecino ya no se acepta aunque la versión del centro coincida.

Prioridades: ni rayon ni `boxel_tasks` las tienen; por eso `JobScheduler` propio (min-heap por
`(prioridad, secuencia)`, tope en vuelo). Los jobs corren en `AsyncComputeTaskPool` a través del
trait `JobExecutor` (`TaskPoolExecutor`; `InlineExecutor` en tests deterministas). Cada hilo usa
un `Mesher` thread-local (~0.6 MiB de scratch) y un `LightScratch` (~0.7 MiB).

---

## 9. Meshing

1. **Gather** (`Mesher::gather`): arma un `PaddedChunk` de 34³ ids de material (u16) más máscaras
   de columnas opacas y transparentes, con el centro completo y del vecino solo lo necesario
   (losas de 32x32 en caras, filas en aristas, un vóxel en esquinas). Clase: `Opaque` oculta caras
   vecinas; `Cutout` y `Translucent` son transparentes para bgm (no ocultan, y bgm omite las caras
   entre dos vóxeles transparentes del mismo material). Los bloques que no son cubos (losas,
   plantas) son `Special`: el greedy los ve como aire y una tercera máscara los marca.
2. **Greedy binario** (`src/greedy.rs`, fork en el repo de bgm 0.5.2 con su aviso MIT): el culling
   de caras de bgm sin cambios y el merge una cara a la vez, comparando una clave por cara en
   lugar del id del vóxel. La clave es el material, el byte de AO y el índice de la paleta de luz
   (ADR 0015); con AO apagado y sin luz es el material y los quads son idénticos byte a byte a los
   de bgm (prueba de regresión). Unos 20 µs por chunk de 32³ en terreno sin AO ni luz (30 a 48x
   más rápido que `block-mesh` greedy).
3. **AO y luz por esquina** (`src/shading.rs`, `AoPolicy`): cada cara visible mira las 9 celdas de
   la capa de enfrente; el AO sale de una tabla de 256 entradas sobre su opacidad y la luz de cada
   esquina es el promedio redondeado de sus 4 celdas sin las opacas. Las 4 luces van a la paleta
   del chunk (hasta 4096 entradas, orden de primera aparición). Ambos dependen solo del vértice,
   así que un quad fusionado es exacto. Los quads salen ya canónicos (esquina mínima, `w` y `h`
   en los ejes u y v de la cara). Validado contra oráculos de fuerza bruta de caras visibles y de
   sombreado.
4. **Bloques que no son cubos** (ADR 0018, ADR 0020): después del greedy, un paso por bloque
   emite sus quads de `w = h = 1` con un código de forma. Losas, escaleras, vallas y antorchas
   son cajas: una cara de caja (`SHAPE_BOX`) lleva en los bits de `w` y `h` el índice de su caja
   en la tabla de `MaterialTable::model_boxes`, que el renderer guarda en el uniform `View`. Una
   cara en el borde se oculta contra un cubo opaco o una cara vecina que la cubre, y se sombrea
   como la parte de la cara del cubo en su lugar. Una cruz son dos quads en las diagonales, en el
   bucket cutout, con la luz de su propia celda.
5. **Fluidos** (ADR 0021): todos los niveles de un fluido se mallan como su fuente. Cada celda de
   superficie calcula la altura de sus 4 esquinas (media pesada de las celdas alrededor, como
   Minecraft). Si las 4 quedan a 14/16 sigue en el greedy (`SHAPE_FLUID_SURFACE`); si no, sale de
   las máscaras del greedy y se emite por bloque con `SHAPE_FLUID_SLOPE` y sus alturas en los bits
   de `w - 1`, `h - 1` y el AO. Validado contra un oráculo de alturas y sin grietas entre chunks.
   Un bloque con agua (ADR 0025) se junta como su agua (`PaddedChunk::waterlogged` guarda el
   bloque, que se dibuja con sus cajas) y su agua va siempre por bloque, con las caras metidas
   1/512 en la celda. Los costados de los fluidos y el techo inclinado llevan la textura de flujo,
   que el shader desplaza con el tiempo (`viewport.z` del uniform `View`). El shader corre la
   profundidad de todo fluido 2^-16 de su distancia hacia atrás: pierde los empates con las caras
   que toca.
6. **Buckets (8, congelados, ADR 0012):** 0 a 5 = caras opacas en orden `Face`; 6 = **cutout**
   (todas las caras de materiales cutout, como las hojas, y el techo inclinado de un fluido opaco,
   ADR 0025); 7 = translúcido. `ChunkMesh` guarda los quads contiguos por bucket,
   `bucket_counts[8]`, la paleta de luz y un hash de todo. En el arena de la GPU la paleta va justo
   después de los quads del chunk (sección 10.2).

**`PackedQuad` (8 bytes):**

```
geo: x:5 | y:5 | z:5 | (w-1):5 | (h-1):5 | face:3 | flags:4 (bit 0 emisivo, bits 1 a 3 forma)
mat: capa:12 | ao:8 (2 bits por esquina, 3 = abierto) | luz:12 (índice en la paleta del chunk)
forma 7 (agua inclinada, w = h = 1): los bits de w-1 y h-1 llevan las alturas de las esquinas 0 y 1
  y el byte de AO las de las esquinas 2 y 3 (4 bits cada una, en dieciseisavos menos 1); el bit
  23 marca el agua de un bloque con agua (caras metidas en la celda)
forma 2: cara de cubo de una celda de fluido que no es superficie (sus costados se animan)
ejes (u, v): ±Y -> (X, Z); ±X -> (Z, Y); ±Z -> (X, Y)
```

Máximo por bucket de cara: 16 384 quads (tablero de ajedrez 3D: 32x32x16). Los buckets cutout y
translúcido juntan las seis caras: hasta 98 304 quads con un solo material por bucket, que es lo
que cubre el buffer de índices compartido. Dos materiales cutout (o translúcidos) alternados no se
tapan entre sí y llegan a 196 608; el registry de M0 tiene uno de cada (riesgo abierto, ver
`informe-m0.md`). Pasado el límite, los tres tiers dibujan solo los primeros 98 304 quads del
bucket. AO: va en la clave del merge en todos los tiers (ADR 0015), por defecto solo en las caras
con algo de luz (`AoPolicy::BakedLitOnly`). Con la luz del mundo cuesta 1.08x de quads en el
overworld a y = 80, 1.00x bajo tierra (las cuevas oscuras se fusionan como sin AO) y 1.74x en el
forest, sin cuevas (gate G1 de P3). El AO en el fragment shader desde ocupación en GPU queda para
High y Mid si las mediciones lo piden.

Hojas en M0: como bgm omite las caras entre dos hojas vecinas, la copa de un árbol se ve como una
cáscara texturada (el pipeline cutout no descarta caras traseras, así que por los huecos se ve la
cara interior del otro lado). Las caras interiores ("hojas elegantes") son una opción de M1 que no
cambia el layout.

---

## 10. Renderer por tier

### 10.1 Principios

- Sin render graph (ADR 0007). Bevy mismo quitó `RenderGraph` en 0.19. Cada tier es una lista fija
  y ordenada de pases sobre recursos explícitos.
- **Frontera de extract**: solo los sistemas `Setup`, `Extract` y `Cleanup` tocan el mundo
  principal. `Extract` copia todo lo que el frame necesita (cámara, estado de ventana, eventos de
  malla o resync completo, pedidos de screenshot) a un recurso privado `ExtractedFrame`; `Prepare`
  y `Draw` leen solo eso y el estado de GPU. Así, mover el render a otro hilo en M3 es un cambio
  local.
- **Reverse-Z** (`Depth32Float`, `CompareFunction::Greater`, clear 0, proyección infinita) y
  **coordenadas relativas a la cámara**: el origen de render es la posición de la cámara
  redondeada a vóxeles. En la CPU, ese origen y los orígenes de los chunks son `i64`; a la GPU
  suben sus 32 bits bajos (`boxel_math::wrap_to_i32`) y el shader los resta en `i32`. La resta
  de WGSL da la vuelta (módulo 2^32), así que el resultado es exacto mientras el chunk esté a
  menos de 2^31 vóxeles de la cámara. Un chunk cargado puede estar más lejos: después de un
  teletransporte, el área vieja sigue cargada unos frames, y con la cámara más allá del límite
  del mundo los chunks quedan en el límite. Su resta daría la vuelta y lo dibujaría cerca de la
  cámara. Por eso solo se dibujan los chunks a menos de 2^30 vóxeles por eje del centro de una
  ventana que sigue a la cámara: cuando el origen de render se aleja más de 2^20 vóxeles de su
  centro, la ventana se mueve. Vale para todos los tiers: la tabla de GPU marca vivo
  (`FLAG_LIVE`) solo a esos chunks y el cull de High salta los demás (al moverse la ventana se
  vuelven a subir todos los slots en uso, en High y Mid, junto con las demás subidas del frame:
  una vez por salto largo o cada 2^20 vóxeles de viaje);
  el culling en CPU (Mid, Low) también salta los de afuera, y resta en `i64` antes de
  pasar a `f32`. Los demás siguen cargados hasta que el mundo los quita. Precisión exacta a
  cualquier distancia del origen del mundo, sin re-basar tablas y sin cambiar el layout de la GPU
  (ADR 0014). El límite siguiente es el `f64` de `Transform` (paso de 1 mm a unos 4.4e12
  vóxeles).
- **Limitación en GL** (verificada en wgpu-hal 30.0.1 y naga): el backend GL siempre remapea z de
  [0, 1] a [-1, 1] (`z * 2 - w`) y no usa `glClipControl`, así que en los tiers sobre GL el
  reverse-Z pierde su ventaja de precisión: con near = 0.1, la resolución de profundidad es de
  unos 0.1 m a 384 m y unos 0.15 m a 500 m. Por eso `near >= 0.1` y el test `gpu_tiers` incluye
  una escena lejana (dos losas a 1 m de distancia a ~480 m) que debe resolverse en todos los
  adaptadores. Un camino con `glClipControl(GL_ZERO_TO_ONE)` (GL 4.5 o ARB_clip_control) se
  propone upstream en M2.
- glam 0.34.0 eliminó `Mat4::perspective_*` y `look_at_*`: se usa `glam::camera`
  (`rh::proj::directx::perspective_infinite_reverse`, `rh::view::look_to_mat4`). El clip space de
  wgpu es la convención DirectX (Y arriba, Z en [0, 1]).

### 10.2 Recursos de GPU compartidos

| Recurso | Uso | Tiers |
|---|---|---|
| Arena de quads | un buffer `STORAGE | VERTEX | COPY_DST` de entradas de 8 bytes, sub-asignado con `offset-allocator`: una asignación por chunk con los 8 buckets contiguos y, a continuación, su paleta de luz (`boxel_render::arena_len`: quads más entradas de paleta, al menos una); 8 Mi entradas = 64 MiB por defecto | todos |
| Tabla de chunks | storage `array<ChunkGpu>` (64 B: origen i32x3 con los 32 bits bajos del origen `i64`, `quad_base`, 8 contadores, flags con el bit vivo solo dentro de la ventana de la cámara, que lee solo High, `light_base` = `quad_base` + quads en el offset 52, `light_len` (entradas de la paleta) en el 56, relleno) | High, Mid |
| Luz por quad | buffer `VERTEX | COPY_DST` paralelo al arena y del mismo tamaño, reservado entero al crear el renderer (64 MiB con el arena por defecto, haya quads o no): en el offset de cada quad, su entrada de paleta ya expandida (segundo atributo de instancia) | Low |
| Tabla CPU | espejo en CPU (origen `i64`, rangos, contadores) para culling en CPU | todos |
| Índice compartido | u32 `[0,1,2,2,1,3] + 4i` para 98 304 quads (2.36 MB) | High, Mid |
| Textura de bloques | `texture_2d_array` RGBA8 sRGB con mips hasta 1x1; el lado de las imágenes del registro (16x16 en el demo), o el tamaño más común de un resource pack (ver abajo) | todos |
| View uniform | 336 B (std140, solo `vec4`): `view_proj`, 6 planos, origen de render, cámara relativa, niebla, contadores, `sky_light` (color del cielo por la luz del día y piso de ambiente), `light_params` (fuerza del AO, ganancia de la luz de bloque) y `light_curve` (brillo de los 16 niveles) | todos |
| Color principal | `Rgba8UnormSrgb` (`RENDER_ATTACHMENT | COPY_SRC | TEXTURE_BINDING`), luego blit a la superficie (alfa siempre 1); facilita screenshots | todos |
| Profundidad | `Depth32Float` | todos |

Texturas de un resource pack (`RenderSettings::textures`, con
`TextureSource::Pack(PackSource::new(dir).with_recipes(recetas))`; `--pack DIR` en los
ejemplos): el pack solo cambia los píxeles de las capas del registro. Ids, orden, mips, sampler y
subida son los mismos, así que los tres tiers lo ven igual.

- El juego pasa una receta por capa (`boxel_pack::LayerRecipe`; las del demo son
  `boxel_demo::pack::recipes`): el bloque de Minecraft cuyo blockstate y modelo dan la textura
  (por ejemplo `cube_column` para tronco arriba y lado), y nombres por defecto si el pack no trae
  modelo (`block/stone`, `block/grass_block_top`...). La variante del blockstate sale de los
  valores de un estado del juego (`boxel:log[axis=y]` lee la variante `axis=y`, ADR 0019). Las
  lámparas del demo usan `glowstone`, `shroomlight`, `sea_lantern` y `redstone_lamp_on`. Una capa
  sin receta prueba `block/<nombre>`.
- Si falta la textura, la capa usa la imagen del registro escalada y se anota un aviso en
  `RenderInfo::textures`.
- Tinte fijo de bioma (llanura) de las recetas del demo, horneado en los píxeles: pasto
  `#91BD59`, follaje `#77AB2F`, agua `#3F76E4`. El lado del pasto compone
  `grass_block_side_overlay` teñido encima.
- Tamaño: el ancho más común de las texturas del pack (potencia de dos, máximo 512); las demás se
  escalan por vecino más cercano. Mips por caja en espacio lineal; las capas cutout guardan alfa
  binario en cada mip sin perder la cobertura del nivel 0 (ADR 0028), las opacas alfa 255.
- Animadas (agua, lava, sea lantern...): se mueven con las reglas de Minecraft (`frametime`,
  lista de cuadros, `interpolate`) al tick `ViewParams::texture_tick`. Cada capa animada guarda
  la capa terminada de cada cuadro y el renderer sube las que cambian con `queue.write_texture`,
  con un presupuesto de bytes por frame. Un flujo animado por el pack no se desplaza (ADR 0028).
- Se carga una vez al elegir la primera GPU (también en una recuperación por pérdida de
  dispositivo se reutiliza). Cuesta unos 20 a 50 ms con caché de disco caliente y 200 a 330 ms
  en frío con los packs de prueba; queda en `RenderInfo::textures::load_time`.

Layouts de bind groups (congelados): grupo 0 estático (textura + sampler), grupo 1 por frame
(view), grupo 2 geometría (arena + tabla, solo lectura, vertex) en High/Mid. Lo pesado y estático
va en el grupo 0, lo que cambia en el más alto (fast path de wgpu sin rebind, #9738).

Si el arena o la tabla se llenan, el chunk no se sube: se cuenta en `RenderInfo::dropped_chunks`,
se avisa en el log y aparece en la línea de diagnóstico `render`. El crecimiento del arena llega
en M1.

### 10.3 Pases por tier

Cada tier usa tres pipelines de vóxeles (ADR 0012, ADR 0018): **opaco** (`fs_opaque`, sin
`discard`, así se conserva el early-Z en todo el terreno, `cull_mode: Back`) para los buckets 0 a
5, **cutout** (`fs_cutout`, alpha test, `cull_mode: None`) para el bucket 6, dibujado después, y
**translúcido** (`fs_translucent`, mezcla alfa, test de profundidad sin escritura, `cull_mode:
None`) para el bucket 7, al final. Los buckets translúcidos salen de una lista en CPU que comparten
los tres tiers, con los chunks de atrás hacia adelante por distancia. Dentro de un chunk, el
renderer reordena los quads translúcidos para la cámara en un trabajo aparte y los vuelve a subir
sobre su rango del arena (ADR 0018, sección 3).

**High** (Vulkan con MDI-count):

1. `clear_buffer(draw_counts)` (dos contadores: opaco y cutout; los bits de la ocultación de
   cuevas que siguen en el mismo buffer no se tocan).
2. Compute `cull_chunks` (64 hilos por grupo, un hilo por slot): salta los slots que la
   ocultación de cuevas oculta (un bit por slot, ADR 0022); AABB contra 6 planos; por bucket
   de cara, un test de backface a nivel chunk (un bucket +Y solo se ve si la cámara está por
   encima de su `min.y`, etc.); el bucket cutout solo usa el frustum. Por bucket visible:
   `atomicAdd` en su lista y escribe `DrawIndexedIndirect { index_count: n*6, instance_count: 1,
   first_index: 0, base_vertex: 0, first_instance: s }` y `DrawRef { chunk_slot, quad_start }` en
   la posición `s`. Cada lista tiene capacidad de hasta 65 535 draws (§5.11).
3. Pase principal: pipeline opaco + `multi_draw_indexed_indirect_count` sobre la lista opaca;
   pipeline cutout + otro sobre la lista cutout; pipeline translúcido + un `draw_indexed` por
   chunk translúcido visible, con su `DrawRef` en un buffer chico que la CPU escribe cada frame.
4. Blit a la superficie.

Ocultación de cuevas (ADR 0022, los tres tiers): en su `Extract`, el render busca desde su cámara
qué chunks se ven a través de espacio abierto (BFS por enlaces de caras, como Minecraft y Sodium)
y el renderer lo pasa a un bit por slot: High lo lee en `cull_chunks`, Mid y Low en el culling en
CPU, y los tres en la lista translúcida. Con el ojo en una cueva oculta de 44 a 77 % de las caras;
desde la superficie solo de 15 a 45 % de las subterráneas (los enlaces de chunks de 32³ dejan
pasar la superficie hacia las cuevas). Con el ojo dentro de la roca no oculta nada.

Hi-Z (oclusión en dos fases) queda para M2. Notas para entonces: con reverse-Z la pirámide usa
reducción **MIN** (lo lejano es 0); `Depth32Float` se lee como textura de profundidad (no
filtrable) y se copia a una cadena de mips `R32Float` en storage; solo aplica a High (GL 3.3 no
tiene compute).

**Mid** (Metal, GL >= 4.3, DX12 por ahora, Vulkan forzado):

1. Culling en CPU con los mismos tests que el shader (también los bits de la ocultación de
   cuevas).
2. Escribe los `DrawRef` del frame en el buffer de instancia.
3. Pipeline opaco y `draw_indexed(0..n*6, 0, i..i+1)` por bucket de cara visible; después
   pipeline cutout para los buckets cutout y pipeline translúcido para los translúcidos.
4. Blit.

**Low** (GL 3.3, GLES 3.0, Vulkan/GL forzado):

1. Culling en CPU (como Mid, con la ocultación de cuevas).
2. Pipeline `vs_instanced` (opaco, cutout y translúcido), arena como buffer de vértices por instancia
   (`Uint32x2` en `@location(0)`, stride 8) y el buffer de luz por quad como segundo buffer por
   instancia (`Uint32x2` en `@location(1)`, mismos offsets: el mismo `first_instance` llega a los
   dos), `TriangleStrip`; por bucket visible: `set_immediates(0, origen del chunk)` y
   `draw(0..4, primer_quad..primer_quad+n)`.
3. Blit.

El bucket translúcido se dibuja desde ADR 0018 (agua y vidrio).

### 10.4 Cómo llega el id de chunk/draw al shader en cada backend

El problema: los builtins de draw no son portables.

- `@builtin(draw_index)` (`SHADER_DRAW_INDEX`) solo existe en Vulkan.
- En DX12, `SV_InstanceID` y `SV_VertexID` **no** incluyen `StartInstanceLocation` ni
  `BaseVertexLocation`; wgpu lo corrige con root constants en draws directos y reescribiendo
  argumentos en la validación indirecta, pero no en la ruta `*_count` ni si se apaga
  `VALIDATION_INDIRECT_CALL`.
- En GL sin `ARB_base_instance`/`ARB_shader_draw_parameters` (NVIDIA 3.3, GLES), `first_instance`
  no llega a `gl_InstanceID` e `INDIRECT_FIRST_INSTANCE` no existe.

La solución de boxel (ADR 0010): **el dato por draw viaja como atributo de vértice de paso por
instancia**, y el hardware lo busca aplicando `first_instance`:

- High/Mid: `@location(0) draw: vec2<u32>` = `DrawRef { chunk_slot, quad_start }` en un buffer de
  instancia; cada draw usa `instance_count = 1` y `first_instance = índice del DrawRef`. El fetch
  de atributos de instancia respeta `first_instance` en Vulkan, DX12 (también en ExecuteIndirect),
  Metal y GL >= 4.2; en draws directos de GL sin base instance, wgpu-hal re-bindea el buffer con
  offset (emulación). Los argumentos indirectos con `first_instance != 0` requieren
  `INDIRECT_FIRST_INSTANCE`, que High exige.
- `base_vertex = 0` y `first_vertex = 0` siempre: `@builtin(vertex_index)` es el valor del índice
  (0..4n) en todos los backends. En DX12 naga lo traduce como `first_vertex + SV_VertexID`, por
  eso este invariante es obligatorio (un test de naga lo vigila).
- Low: el quad **es** el atributo de instancia y el origen del chunk va en **immediates**.
- Prohibidos en todos los WGSL (test con naga): `instance_index`, `draw_index`, `num_workgroups`,
  `base_instance`, `base_vertex`.

Validado (con el layout anterior de 7 buckets): High, Mid y Low producen la misma imagen en
NVIDIA, RADV y lavapipe por Vulkan, y Mid/Low en radeonsi y llvmpipe por GL; Low en NVIDIA GL
3.3. Con el bucket cutout el método es el mismo; `gpu_tiers` lo vuelve a verificar en todos los
adaptadores.

### 10.5 Pitfall de GL 3.3: un solo uniform block (ADR 0011)

Medido con wgpu 30.0.1 en el contexto 3.3 de NVIDIA: un pipeline con **dos** uniform blocks lee
los datos de la view en ambos. Causa (wgpu-hal 30.0.1): `SHADER_BINDING_LAYOUT` se activa con
`supports_compute`, que en ese contexto es verdadero por extensiones; entonces wgpu omite el
remapeo `glUniformBlockBinding` posterior al link, pero naga en GLSL 330 no puede escribir
`binding = N`: todos los bloques quedan en el binding 0. Regla del tier Low: **exactamente un
uniform block y un par textura/sampler por pipeline**; el origen del chunk va en immediates.
Hay que reportarlo upstream junto con la regresión #9169.

### 10.6 Shaders

WGSL plano en `boxel_shaders/shaders/`, composición por concatenación (`common.wgsl` se antepone):

| Módulo | Entry points | Tiers | GL mínimo |
|---|---|---|---|
| `voxel_pull` (common + pull) | `vs_pull`, `fs_opaque`, `fs_cutout` | High, Mid | 4.3 / ES 3.1 |
| `voxel_instanced` (common + instanced) | `vs_instanced`, `fs_opaque`, `fs_cutout` | Low | 3.3 / ES 3.0 |
| `cull` (common + cull) | `cull_chunks` | High | 4.3 / ES 3.1 |
| `blit` | `vs_fullscreen`, `fs_blit`, `fs_blit_encode_srgb` | todos | 3.3 / ES 3.0 |

Validados en `cargo test` con naga 30.0.1 contra **todos** los backends que wgpu usa en
escritorio: GLSL en el piso de cada módulo, MSL 2.1 y 2.4, HLSL SM 5.1 (FXC) y SM 6.0 (DXC) y
SPIR-V, además de layouts y reglas. Así se cubren DX12 y Metal sin hardware. Reglas: niveles de
mip de `textureLoad` en `i32` (bug wgpu#10037), nada de `enable subgroups;`, `var<immediate>` (no
`var<push_constant>`). WESL se reevalúa en M2. Los tests de naga también fijan el layout de
`View` (336 B) y `ChunkGpu` (`light_base` en el offset 52, `light_len` en el 56), los nibbles de
`LightValue`, que Low recibe exactamente dos atributos de instancia (el quad y su luz) con un solo
uniform block y que ningún shader guarda un array en una variable local.

### 10.7 Luz en el fragment shader y día y noche (ADR 0015)

Cada quad lleva el índice de su entrada en la paleta de luz del chunk: las `LightValue` (cielo,
rojo, verde, azul, 4 bits cada uno) de sus 4 esquinas. High y Mid la leen del arena en
`ChunkGpu::light_base` más el índice, sin binding nuevo; Low la recibe ya expandida como segundo
atributo de instancia. Un índice fuera de la paleta (solo en una malla mal hecha) da cielo
abierto en los tres tiers y en la CPU: High y Mid lo comparan con `ChunkGpu::light_len` y nunca
leen los datos de otro chunk. El vertex shader pasa la entrada, el byte de AO y la cara como valores
planos, y (s, t), la posición dentro del quad (la esquina k está en (k & 1, k >> 1)).

El fragment shader, igual en los tres tiers:

1. Interpola bilinealmente cada canal de las 4 esquinas en (s, t) (en la forma `a + (b - a) * s`,
   exacta cuando las 4 esquinas son iguales) y pasa cada canal por la curva de brillo
   (`light_curve`, 16 niveles, interpolación lineal, indexada en el uniform sin copiarla).
2. `luz = max(curva(cielo) * sky_light.rgb, curva(rgb) * ganancia, ambiente)`.
3. `color = albedo * sombreado de cara * mix(1 - fuerza_ao, 1, ao / 3) * luz`, con el AO también
   interpolado bilinealmente, y después la niebla.

Con cielo 15 bajo un cielo blanco de mediodía y AO abierto da exactamente la imagen anterior a la
luz. La curva por defecto es la de Minecraft, `f / (4 - 3f)` con `f = nivel / 15`, con un ajuste
de gamma (`RenderSettings::light_gamma`); la fuerza del AO (0.55) y la ganancia de la luz de
bloque (1) también están en `RenderSettings`.

Día y noche no remallan ni suben nada: `boxel_world::DaylightPlugin` (en `DefaultPlugins`) avanza
`TimeOfDay` un tick por paso fijo (64 Hz; un día de 20 minutos son 76 800 ticks, el tick 0 es la
salida del sol), y `DaylightCurve` da, según la altura del sol, el color de la luz del cielo (día
blanco a 1.0, atardecer cálido a 0.6, noche azul a 0.2), el piso de ambiente (0.02 a toda hora:
las cuevas quedan casi negras) y el color del cielo, que es también el de la niebla. El `Extract`
del render arma con eso la `SceneLight` del frame; sin el plugin queda un mediodía con
`RenderSettings::clear_color`. La luz de bloque no depende de la hora: una cueva cerrada se ve
igual al mediodía y a medianoche (lo verifica `gpu_terrain`).

La niebla de distancia (`RenderSettings::distance_fog`, issue #51) funde el terreno con el color
del cielo solo al final, como Minecraft: el último 10 % de la distancia de vista, como mucho 64
bloques. Con 512 bloques va de 460,8 a 512; con 1024, de 960 a 1024. Es esférica (distancia 3D),
porque el mundo también es infinito hacia arriba y abajo. El juego cambia la fracción y el tope,
o la apaga (`enabled: false`): el borde queda nítido. Las capas de `FogPlugin` con
`FogFade::Scene` usan ese mismo rango; con la niebla apagada no se desvanecen y llegan a su
`max_distance`.

La niebla solo tapa el borde de lo mallado si la distancia de vista no pasa de donde el disco de
chunks está completo (`ChunkViewer::mesh_reach`). El disco se mide en chunks (`dx² + dz² <= r²`),
así que su borde es una escalera que hacia las diagonales queda más cerca que 32 bloques por
chunk de radio. Con la cámara en una esquina de su chunk, el primer hueco está a 340 bloques a
radio 12 (no a 384) y a 472 a radio 16 (no a 512). Con la distancia en 32 por radio esos huecos
quedan casi sin niebla y se ven muescas duras que cambian al moverse. Por eso `voxel_world`,
`snapshot` y `physics_sandbox` usan `mesh_reach` como distancia de vista (lo verifica
`gpu_tiers`). `stress` y `headless_world` siguen en 32 por radio para que sus números sigan
comparables. Ver los 512 bloques completos a radio 16 pediría mallar un anillo más, como el
margen de un chunk que suma Minecraft, con su costo.

### 10.8 Niebla y nubes cúbicas (ADR 0030)

Opcionales: solo existen si el juego agrega `FogPlugin` (fuera de `DefaultPlugins`). Sin el plugin
no hay pipeline, target ni módulo de shader, y la profundidad principal no se puede leer como
textura: los frames son los de antes, byte a byte.

Con el plugin, `VoxelRenderer::render` agrega dos pasadas después del pase principal y antes de
los overlays y el blit, iguales en los tres tiers (`fog.wgsl`, módulo propio):

1. **Marcha** a un target `Rg32Uint`, a media resolución por defecto. Cada rayo recorre las
   celdas de cada capa con un DDA hasta la profundidad opaca y acumula `exp(-densidad * largo)`.
   La cobertura de cada celda sale de un ruido entero (exacto a cualquier distancia, ADR 0014) y
   del momento de `FogClock` (el tick y la fracción del siguiente), con una rampa suave: las
   celdas aparecen y se apagan de a poco. Cerca de la cámara la capa puede ser suave, sin bordes
   de celdas. Guarda un color y una opacidad por píxel del bloque de 2x2.
2. **Composición** sobre el color principal (`dst * T + C`). Descarta los píxeles sin niebla.

Cada pipeline liga a lo sumo un uniform block y una textura, sin sampler (ADR 0011). El uniform
`Fog` (1008 bytes) se escribe cada frame; no se sube nada más. El estilo (capas, cubos, colores,
viento) es del juego; los presets del ejemplo están en `boxel_demo::fog`. La niebla detrás del
agua o del vidrio se pinta encima de esa superficie (límite conocido).

### 10.9 Terreno lejano: LOD por anillos (ADR 0031)

Opcional, como la niebla: solo existe si el juego agrega `LodPlugin` (fuera de `DefaultPlugins`).
Sin el plugin no hay recursos, sistemas, trabajos, pipeline ni módulo de shader, y los frames son
los de antes, byte a byte.

- **Nodos.** Un nodo de nivel `k` (1 a K) son 32³ celdas de `2^k` bloques, con la `ChunkKey` de
  siempre (`lod = k`). Un octree 3D con el mismo radio `R` en cada nivel arma anillos hasta el
  horizonte `H` (12.288 bloques en el preset `far`: `R = 24`, `K = 4`).
- **Datos.** El generador muestrea cada nivel (`WorldGenerator::lod`, `LodSource`): un trabajo
  arma una columna de un nivel y malla sus nodos con el mallador de siempre (todo cubo opaco, sin
  luz ni AO) más cortes por octante, faldones y, en el nivel 1, paredes de agua. La regla de la
  celda: gana cualquier sólido, salvo agua sobre el suelo.
- **Ediciones.** Los chunks editados (y, con `PersistPlugin`, los guardados) tienen sus celdas de
  los niveles 1 a 5 calculadas desde sus bloques (una pirámide por chunk) y puestas sobre las del
  generador; el nodo viejo se dibuja hasta que llega el nuevo, y dos vecinos del mismo nivel que
  se rearman por una edición cambian juntos. Un chunk editado sobre la última capa del generador
  sube el techo de su columna. Sin persistencia, un chunk editado que se descarga vuelve a ser el
  del generador. Un generador sin `lod()` tiene solo el nivel 1, armado desde sus chunks reales.
- **Cobertura.** Cada nodo lleva una máscara de octantes ocultos donde dibuja el detalle completo
  o un nivel más fino, sacada de los `ChunkMeshEvent` leídos completos cada frame: el LOD y el
  detalle completo nunca dibujan el mismo volumen y cambian en el mismo frame.
- **Dibujo.** `LodPass` en `VoxelRenderer`, con su propia arena, justo después de lo opaco y
  cutout. En High, desde la GPU: una tabla de nodos, un compute que descarta nodos y caras que no
  se ven (y, cuando sirve, los que tapa el terreno cercano) y una lista indirecta por segmento.
  En Mid, Low y las GPUs integradas, instanciado (`lod.wgsl`), una llamada por tramo de caras
  que miran a la cámara. Los faldones y las paredes de agua solo se dibujan donde hay una
  costura con otro nivel o con el detalle completo. Las mismas imágenes en los dos caminos. Con
  el LOD dibujando, el plano lejano del recorte de chunks y de la búsqueda de cuevas
  pasa a `H` (los chunks publicados llegan más allá de la distancia de vista) y la niebla de
  distancia se mide sobre `H` con la misma regla (10.7); las capas de `FogPlugin` conservan su
  alcance. Apagado en GL.
- **Costo.** En la RTX 5050, `far` suma de 0,54 a 0,60 ms de GPU por frame y ningún frame de más
  de 6,94 ms; en el hilo principal casi nada (`render.lod` 0,007 ms de media). El terreno lejano
  se llena en 1,1 a 1,7 s. En la 780M, `near` cuesta unos 0,4 ms y `far` 1,1 ms. Detalle en
  `docs/rendimiento/lod-2026-10.md`.

---

## 11. Modelo de hilos y presentación

| Hilo | Qué corre |
|---|---|
| Principal | event loop de winit, `App::update()` completo (todos los schedules, incluido `Render`), selección de GPU, creación de superficies, submit, acquire y present |
| `ComputeTaskPool` (7 con 16 hilos) | sistemas en paralelo del ejecutor multi-thread del ECS, `par_iter` |
| `AsyncComputeTaskPool` (6) | generación y meshing de chunks (jobs con tope en vuelo) |
| `IoTaskPool` (2) | persistencia futura, archivos |
| wgpu interno | callbacks de device lost/errores (pueden llegar desde otro hilo: por eso `GpuHealth` es atómico) |

La ventana es un **recurso no-send** (`NonSend<PrimaryWindow>`): el ejecutor corre en el hilo
principal todo sistema que la lee. Motivo: en macOS cada método de ventana de winit llamado fuera
del hilo principal hace un dispatch síncrono a la cola principal, y en Windows varios
(`set_cursor_grab`, `set_cursor_visible`) esperan al hilo del event loop; como el hilo principal
está bloqueado dentro de `App::update()` mientras los workers corren sistemas, eso es un
deadlock. En macOS además `window_handle()` falla fuera del hilo principal.

Los error scopes de wgpu son por hilo: toda la puesta en marcha de GPU (selección, smoke tests,
`VoxelRenderer::new`) corre en un hilo. La inicialización asíncrona de M3 tiene que respetarlo.

**Presentación gobernada por frame callbacks (ADR 0008).** Hechos verificados: winit 0.30.13 nunca
envía `WindowEvent::Occluded` en Wayland ni en Windows; en Wayland, después de
`pre_present_notify()`, winit retiene `RedrawRequested` hasta que el compositor entrega el frame
callback, y una superficie oculta no recibe frame callbacks. Diseño:

- `app.update()` se maneja desde `about_to_wait`, nunca desde `RedrawRequested`.
- `RedrawRequested` solo abre la compuerta `WindowState::frame_ready`. El renderer adquiere y
  presenta únicamente con la compuerta abierta, llama `pre_present_notify()` justo antes de
  presentar y la cierra; el runner pide el siguiente redraw.
- Si un redraw pedido no llega en 100 ms, la ventana se considera oculta (`occluded`) y el runner
  limita la simulación a 30 Hz con `WaitUntil` (sin esto, con `Poll`, una ventana oculta gira a
  ~200 000 updates por segundo).

Medido con una ventana minimizada durante 2 s: presentando en cada update con FIFO, GL se congeló
(0 updates) y Vulkan quedó limitado a ~105 updates/s; con la compuerta, Vulkan (NVIDIA, RADV) y GL
(radeonsi) siguieron actualizando sin presentar nada. Con la ventana visible, la compuerta
presenta como mucho una vez por refresco del monitor.

**Límite de FPS y vsync (ADR 0013).** El runner no usa `Poll` salvo sin límite:

- `FrameLimit { max_fps }` (recurso de `boxel_app`, 144 por defecto, 0 = sin límite) se lee antes
  de cada frame. `FramePacer` calcula deadlines: el frame siguiente vence un período después de
  que venció el anterior (sin deriva), y un frame atrasado más de un período reinicia la grilla
  (sin ráfagas). La espera usa `WaitUntil` hasta 300 µs antes del deadline y cede el hilo en un
  bucle el resto. Los eventos que despiertan el bucle antes se traducen a mensajes enseguida.
- Ventana oculta: el período es el mayor entre el límite y 33 ms (30 Hz como antes).
- Wayland: si un frame vence con la compuerta cerrada, espera el frame callback hasta un cuarto
  del período (2 ms como máximo) y, si llega, reinicia la grilla ahí. Así las updates se
  enganchan al reloj del compositor. Medido con el límite igual al panel (144 Hz): sin esto se
  mostraban 113 a 126 frames por segundo; con esto, 136 a 143, con 143.6 updates por segundo.
  El costo: con render, las updates pueden quedar hasta un 2 % abajo del límite (119 con 120,
  127.5 con 130, 143.6 con 144; 60 y 100 exactos) y los frames mostrados unos pocos por ciento
  abajo aunque el límite esté por debajo del panel (111 a 119 con 120).
- Con el límite en el 99 % de la frecuencia del monitor o más (ADR 0017), en Wayland el ritmo lo
  marca el monitor (`MonitorHandle::refresh_rate_millihertz`): cada update corre apenas llega su
  frame callback, y con la compuerta cerrada lo espera hasta un refresco más un cuarto de refresco
  (con tope de 2 ms para ese cuarto), lo que tarda el de un refresco que el compositor salteó.
  Solo si los redraws de verdad esperan al compositor (2 callbacks seguidos) y los callbacks
  llegan a la frecuencia que dice winit: una ventana sin render sigue con la grilla del límite. `FramePacingInfo` dice en
  cada update cómo se marcó el ritmo y el overlay F3 muestra `paced by the display`.
- La espera no es solo dormir (ADR 0023): si la app tiene sistemas en `Idle` y la update pidió
  pasos (`IdleWanted`), el runner corre pasos de 0.5 ms como máximo en las iteraciones de la
  espera, hasta 1 ms antes del deadline (o 0.7 ms antes de su propio despertar). La iteración de
  la update pone `Poll` en vez de correr un paso, así un redraw que llega enseguida se marca
  antes. Entre pasos se atienden eventos y la compuerta: un paso nunca retiene una update. Sin
  pedido, la espera es la de siempre.
- `PresentSettings { mode }` (recurso de `boxel_render`) cambia el vsync en tiempo de ejecución:
  `Setup` reconfigura la superficie con el mismo dispositivo. Por defecto vsync apagado:
  `Mailbox`, si no `Immediate`, si no `Fifo` con una nota en el log. Así manda el límite.
- Frames mostrados: con vsync, como mucho la frecuencia del monitor (y en X11 las updates
  también bajan a ella, porque presentar bloquea; Windows no se midió). Sin vsync, hasta el
  límite, salvo en Wayland, donde la compuerta deja presentar una vez por refresco con o sin
  vsync, y por debajo del panel pueden quedar unos pocos por ciento abajo del límite (ver arriba).

| RTX 5050, panel de 144 Hz, radio 12 | Updates/s | Mostrados/s | CPU (un núcleo = 100 %) |
|---|---|---|---|
| Wayland, 144, Mailbox | 143.6 | 136 a 143 | 20 % |
| Wayland, sin límite, Mailbox | 11 000 a 16 000 | 139 a 143 | 150 % |
| Wayland, 144, Fifo | 143.6 | unos 107 | 17 % |
| X11 (XWayland), 144, Immediate | 144.0 | 144.0 | 17 % |
| X11 (XWayland), sin límite, Immediate | 1477 | 1477 | 90 % |
| X11 (XWayland), 144, Fifo | 143.4 | 143.4 | 25 % |

Con Fifo en Wayland, NVIDIA y RADV muestran unos 107 de 144 con o sin límite (la combinación de
Fifo con la compuerta). La Radeon 780M con Mailbox muestra 143.3. GL (radeonsi) solo ofrece Fifo.

En GL, wgpu reproduce los comandos en el hilo que hace submit, así que los frames GL pagan el
grabado dos veces (no medido aún). El render en un hilo propio con un mundo de render (`SubApp` +
extract) es M3; la frontera de extract ya existe.

---

## 12. Persistencia (ADR 0024)

`boxel_world::persist` guarda y carga el mundo mientras se transmite:

```rust
pub trait ChunkStore: Send + Sync + 'static {
    fn load(&self, key: ChunkKey) -> std::io::Result<Option<Vec<u8>>>;
    fn keys(&self) -> std::io::Result<Vec<ChunkKey>>;
    fn load_meta(&self, name: &str) -> std::io::Result<Option<Vec<u8>>>;
    fn save_batch(&self, batch: &SaveBatch) -> std::io::Result<()>; // chunks y metadatos, atómico
    fn flush(&self) -> std::io::Result<()> { Ok(()) }
}
```

- Stores: `RedbStore` (un archivo `world.redb` por mundo, redb 4.3, Rust puro, ACID),
  `MemoryStore` (tests y herramientas) y `NullChunkStore` (sin guardar: el mundo de antes).
- Clave de chunk: dimensión, lod y x, z, y como `i64` en big endian con el signo invertido (27
  bytes): exacta en todo `i64` y con las capas de una columna juntas.
- Formato versionado (`FORMAT_VERSION` 1): los bloques se guardan por el nombre de su estado
  (ADR 0019) a través del IdMap del mundo; registros con magia, versión, suma de control y LZ4.
- Se guardan solo los chunks editados (el generador rehace el resto), los ticks programados
  pendientes y el reloj. Los techos de cielo no se guardan: la caminata de una columna lee los
  chunks guardados.
- `WorldStore` es el mundo abierto (store, IdMap, copias pendientes); `PersistPlugin` va después de
  `WorldPlugin`. Un chunk guardado se lee en su job de generación; uno editado que se descarga se
  entrega a una copia en memoria y un job en `IoTaskPool` lo escribe (uno por vez, hasta 256
  chunks por transacción). Autoguardado cada 60 s y guardado final en `Shutdown`. Los ticks y el
  reloj van siempre en la misma transacción que todos los chunks editados: el disco guarda un solo
  momento del mundo.
- Si el disco falla, los chunks esperan en memoria y `RedbStore` vuelve a abrir el archivo cuando
  el disco funciona otra vez.
- Entidades: las marcadas con `Saved` se guardan con su chunk a través de los `EntityCodec` que
  registran los plugins (la física guarda sus cuerpos con `BodyCodec`). Se guardan y despawnean
  cuando su chunk se descarga, y vuelven con él antes del paso de física siguiente. El
  autoguardado y la salida las guardan en la misma transacción que los chunks con ediciones, los
  ticks y el reloj, y solo rearman las que cambiaron (ADR 0024 sección 3).

---

## 13. Diagnóstico y profiling

- Logs: `tracing` + `tracing-subscriber` (`RUST_LOG`, filtro por defecto
  `info,wgpu_core=warn,wgpu_hal=warn,naga=warn,sctk_adwaita=error`), puente de `log`. El
  último silencia los avisos cosméticos de las decoraciones de ventana de winit en Wayland
  (`Ignoring unknown button type`, por el layout de botones de GNOME); sus errores siguen saliendo.
- Nombres en los diagnósticos del ECS: feature `debug` de la fachada (activa por defecto).
- `FrameStats` (frame time, EMA, fps, peor frame) y `DiagnosticsBoard` con líneas nombradas:
  `gpu` (resumen de selección), `render` (tier, chunks, quads, draws, visibles, descartados),
  `world` (chunks por estado, cola, en vuelo, fallidos). `LogDiagnosticsPlugin` las imprime cada
  2 s. Su `fps=` cuenta updates de la App, no frames mostrados.
- Overlay F3 (`boxel_debug`): frames mostrados por segundo (de `RenderInfo::frames_presented`) y
  updates por segundo por separado, tiempo de frame, límite, vsync y modo de presentación, GPU,
  cámara, la luz (cielo y RGB) en el vóxel del ojo y en `CrosshairTarget`, la hora de
  `TimeOfDay` y contadores de chunks. `CrosshairTarget` lo llenan las apps que apuntan bloques
  (`voxel_world` pone la celda frente a la cara apuntada); sin eso solo sale la luz del ojo.
  Teclas opcionales `FrameKeys` (F5 límite, F6 vsync). Las apps suman líneas propias con
  `OverlayLines` (`voxel_world` muestra el bloque apuntado, la selección, el modo, con el estado
  del jugador si camina, y la línea de física) y `Crosshair` dibuja una mira en el centro.
- Línea `physics` de `DiagnosticsBoard` (`PhysicsStats`): cuerpos, dinámicos despiertos,
  congelados, dormidos, pares del broadphase, empujes, solapes que avisan, impactos, proyectiles
  y sus impactos, y tiempos del último paso de física (el movimiento y lo de entre cuerpos).
- `FrameTimeHistory`: los últimos frames (4096 por defecto, mínimo 1024) con su tiempo real
  (`Time::real_delta`); `summary()` da p50, p95, p99, p99.9, máximo, promedio y 1% low.
- Tirones mientras se carga el mundo: `examples/stress.rs` y `just bench-*`
  ([benchmarks.md](benchmarks.md)).
- Línea estable para scripts: `boxel: backend=Vulkan tier=High class=Discrete adapter="..."
  driver="..."` al estar listo el GPU, y `DECISION: ...` al final del reporte de selección.
- Tracy: feature `trace_tracy` (tracing-tracy 0.12.0 + tracy-client 0.19.0, GUI 0.14.1) y
  `profiling/profile-with-tracing` para los scopes internos de wgpu.
- GPU timestamps (wgpu-profiler): M1, después de verificar compatibilidad con wgpu 30.
- RenderDoc: no soporta Wayland; `just renderdoc` corre bajo XWayland.
- `gpu_info` (en `boxel_gfx`, compila sin el resto del motor) como herramienta de campo: lista
  adaptadores, clasificación con razones, puntajes, smoke tests, decisión, y muestra o borra el
  LKG de una app.

---

## 14. Estrategia de testing

| Nivel | Qué | Dónde |
|---|---|---|
| Contrato (esqueleto) | lo que el esqueleto ya implementa: App, tiempo fijo y mensajes, task pools, frustum, registry, `ChunkData`, `tier_request` en los umbrales, superficie, derives por la fachada | `tests/contract.rs`, `tests/derives.rs` |
| Unitarios | lógica pura de cada crate (paleta, scheduler, preferencias, clasificación con `AdapterCaps` sintéticos, scoring, LKG en directorio temporal, input, plugins) | `cargo test` |
| Propiedades | paleta contra un array denso, mesher contra el oráculo de fuerza bruta (opaco, cutout, translúcido), enlaces de caras y búsqueda de la ocultación de cuevas contra fuerza bruta | proptest |
| Shaders | naga: parse + validación, GLSL en el piso de cada módulo, MSL, HLSL (FXC y DXC), SPIR-V, builtins prohibidos, constantes, layouts, regla de Low | `boxel_shaders/tests/naga.rs` |
| GPU | contrato (readback, PNG, scopes, `tier_request` en adaptadores reales); selección headless y smoke por tier; misma escena en todos los tiers de cada adaptador (<= 0.1 % de píxeles distintos), escena lejana de profundidad; terreno real; los mismos píxeles con y sin ocultación de cuevas | `tests/gpu_*.rs`; CI con lavapipe y llvmpipe; un hilo por binario |
| Integración | `voxel_world` automatizado (`--frames`, `--timeout`, `--compare-to`, `--minimize-test`, `--resize-test`, pérdida de dispositivo), `window_smoke`, `headless_world`, test de ambigüedades de schedules | plan de verificación |
| ECS vendorizado | tests de librería y doctests de upstream con lockfile fijo; Miri en PRs del fork y semanal | `vendor/` |
| Repositorio | superficie del ECS, ningún crate `bevy*`, lockfiles sincronizados, cargo-deny, typos | `just lint`, `just deny`, CI |
| Benchmarks | criterion: paleta, meshing, worldgen (informativos en M0) | `benches/` |

---

## 15. Roadmap

| Milestone | Contenido | Criterio de salida |
|---|---|---|
| **M0 scaffold** | workspace y tooling; fork del ECS; `boxel_app`; ventana con compuerta de presentación e input; detección completa (tiers, scoring, smoke, overrides, LKG de crashes, device lost, catch_unwind); tiers High, Mid y Low con buckets opaco y cutout (High sin Hi-Z); paleta, registry, worldgen, mesher, streaming con jobs; cámara libre; `gpu_info`, `voxel_world`, `headless_world` | compila, tests, clippy y rustdoc limpios; `voxel_world` dibuja terreno en Vulkan High y en GL (Mid y Low); imágenes iguales entre tiers; la simulación no se congela con la ventana minimizada |
| **M1 mundo jugable** | criterio de entrada: re-fork sobre bevy 0.20.0 final (hecho el 2026-10-09, issue #24). Motor sin contenido, con el de los ejemplos en `boxel_demo` (hecho: ADR 0029, issue #48). Niebla y nubes cúbicas como plugin opcional (hecho: ADR 0030, issue #47; falta el mapa del suelo). Pase translúcido; AO; raycast y edición de bloques (hechos: ADR 0016, `voxel_world`); el sistema de ediciones si se recortó; propiedades de bloque e IdMap; `ChunkStore` con redb (hecho: ADR 0024, con entidades); crecimiento del arena y presupuesto de subida; caras interiores de hojas (opcional); re-selección en caliente si se recortó; decisión `static-dxc` o DLL en Windows; timestamps de GPU; actualizar a wgpu 31 | editar y guardar un mundo; agua visible |
| **M2 GPU-driven completo** | Hi-Z en dos fases (High) o enlaces por subceldas de 16³ (el cave culling por BFS ya está en los tres tiers, ADR 0022); pool de ocupación en GPU; `create_shader_module_trusted` con medición; pipeline cache de Vulkan; verificar DX12 High; clip control en GL (upstream); decidir WESL | caída de draws y de tiempo de GPU medida |
| **M3 escala** | LOD por anillos 3D con el generador muestreando cada nivel (M1, M2 y M3 del LOD hechos: ADR 0031; falta la prueba del dueño); listas indirectas segmentadas (más de 65 535 draws); render en hilo propio; inicialización de GPU asíncrona (respetando los error scopes por hilo) | distancias de vista grandes estables |
| **M4 experimental** | ray marching sobre bricks/ocupación; mesh shaders opt-in; evaluar bevy_ecs 0.21 y el núcleo ECS propio | prototipos medidos |

wgpu v31 (milestone con fecha 2026-10-09) trae cambios que tocan el scaffold
(`DeviceDescriptor::default_queue`, separación de `BufferUsages`/`ShaderStages`,
`Limits::max_storage_buffers_in_vertex_stage`, `DownlevelFlags::LINEAR_INTERPOLATION`). Se
actualiza al empezar M1, con `boxel_gfx` como frontera.

---

## 16. Riesgos

| Riesgo | Impacto | Mitigación |
|---|---|---|
| wgpu rompe API cada ~3 meses (v31 inminente) | migraciones frecuentes | pin `=30.0.1`, `boxel_gfx` como frontera, una actualización por milestone, tests de GPU en CI |
| GL de NVIDIA depende de workarounds de bugs de wgpu (#9169, `SHADER_BINDING_LAYOUT`) | Low podría romperse en otra versión | clasificación por versión GL, regla de un uniform block, smoke `Draw` en GL con pipelines reales, test local en NVIDIA GL, reportar upstream |
| DX12 y Metal sin hardware de prueba | errores no vistos | DX12 tope Mid, naga valida MSL y HLSL en cada `cargo test`, CI en Windows y macOS con `gpu_info` informativo |
| Reverse-Z pierde precisión en GL | z-fighting lejano en Mid/Low sobre GL | near >= 0.1, escena lejana en `gpu_tiers`, clip control en M2 |
| Recuperación de dispositivo cambiando de backend en la misma ventana nunca ejercitada | crash posible en recuperación | superficie nueva por selección; recorte a salida limpia si falla |
| Tier Low en drivers GLES 3.0 sin vertex attrib binding nunca ejecutado | emulación de `first_instance` sin probar | documentado; probar si aparece hardware |
| Fork de `bevy_ecs` sobre un RC | API cambia en 0.20 final, fixes con retraso | re-fork por script (segundos) como criterio de M1, parches upstream, Miri en PRs, superficie curada |
| `ParamSet` necesita `&'static` | sorpresa para usuarios | documentado en la fachada con el texto exacto del error |
| glam 0.34.0 salió el mismo día del diseño y rompió API | fricción con crates de terceros | pin `=0.34.0`; volver a 0.33.12 si un crate necesario lo exige (cambio acotado a `boxel_math`) |
| binary-greedy-meshing: crate chico, MIT, sin AO | dependencia frágil | fork en el repo (`boxel_mesh/src/greedy.rs`, ADR 0015) con oráculos y una prueba de regresión byte a byte contra el crate |
| Arranque lento en laptops híbridos (despertar la dGPU ~1 s) | mala primera impresión | documentado; inicialización asíncrona en M3 |
| Más de 65 535 buckets visibles en High | huecos, también cerca | aviso; segmentación en M3 |
| La ocultación de cuevas deja pasar la superficie hacia las cuevas (enlaces de chunks de 32³) | desde la superficie oculta poco (15 a 45 % de las caras subterráneas) | medido (`docs/rendimiento/cuevas-fase4-2026-10.md`); subceldas de 16³ o Hi-Z en la fase 5 |
| Implementación en paralelo con varios agentes | bloqueos y pisadas | copias privadas por paquete, compuerta de propiedad, elementos transversales ya implementados en el esqueleto, protocolo de bloqueo con el lead |
| `/tmp` es tmpfs de 12 GB con cuota | builds que fallan, insumos que desaparecen | target dirs en disco; el diseño se copió al repo durante M0 |

---

## 17. Evidencia

- Investigación previa: mediciones de wgpu contra ash, capacidades por backend, detección con
  salida real de sondas, datos de vóxeles, meshing, ECS (forks y alternativas) y tooling. Las
  cifras de este documento salen de ahí.
- Sonda de render del arquitecto (usada durante M0, no está en el repo): oráculo del mesher, tres rutas con `DrawRef`, comparación de imágenes en 6 configuraciones
  de adaptador, naga GLSL. Descubrió la colisión de uniform blocks en NVIDIA GL 3.3.
- Sonda de ventana oculta (usada durante M0, no está en el repo): FIFO ingenuo contra compuerta por
  frame callbacks, en Vulkan y GL, ventana minimizada (§11).
- Tres revisiones adversariales del borrador v1 (gráficos, ECS y arquitectura Rust, entrega en
  paralelo). Cada observación quedó resuelta o rechazada con motivo en el registro de revisión de
  la especificación del scaffold.
- Esqueleto compilado: API congelada de los 15 crates propios más `boxel_facade_check`;
  `cargo check`, `cargo test` (39 tests en 42 objetivos, 0 fallos), clippy con `-D warnings`, rustdoc, `cargo fmt`,
  cargo-deny, MSRV 1.97.1 y los chequeos del repositorio, todos limpios; `refork.sh` regenera
  `vendor/` y sus tests pasan con lockfile fijo.

---

## 18. Cómo se implementa el M0

El M0 lo construyen varios agentes en paralelo. Para que no se pisen:

- **Fases:** un agente prepara el repositorio (WP0); siete agentes trabajan en paralelo, uno por
  grupo de crates (fase A); dos agentes conectan las piezas (fase B: lado GPU/ventana y lado
  CPU/integración); un agente verifica todo y limpia (fase C).
- **Copias privadas:** cada agente trabaja en su propia copia del repositorio y solo ve las
  versiones del esqueleto de los crates de otros, que son el contrato congelado.
- **Compuerta de propiedad:** al terminar cada fase, el lead copia al repositorio solo las rutas
  que cada paquete posee y rechaza cualquier cambio fuera de ellas.
- **Esqueleto ejecutable:** lo que varios paquetes necesitan (frustum, tiempo, cámara, pedido de
  dispositivo, registry, tests de shaders) ya viene implementado y probado, así nadie espera a
  nadie.
- **Recortes previstos:** si falta presupuesto, primero se recorta la re-selección en caliente
  tras perder el dispositivo, luego el sistema de ediciones; los benchmarks son informativos.
