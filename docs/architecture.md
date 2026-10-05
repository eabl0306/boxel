# boxel: arquitectura

Documento técnico de referencia. Fecha: 2026-10-05. Estado: aprobado para el milestone M0
(revisión final, después de tres revisiones adversariales del borrador v1).
Las decisiones puntuales están en `docs/adr/` (0001 a 0012). Durante M0 hubo además una
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
- **ECS:** un fork vendorizado y recortado de `bevy_ecs` 0.20.0-rc.2 (crates `boxel_ecs*`, cero
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
cargo-deny, el MSRV 1.97.1 y los tests del ECS vendorizado (961 + 480) con lockfile fijo.

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

### No objetivos (por ahora)

- Web (WebGPU/WebGL2) y móvil. El diseño no los impide (tiers Mid/Low cubren WebGPU/WebGL2) pero
  no se compila ni se prueba para ellos.
- Un RHI propio o backends crudos de Vulkan/GL (ADR 0001).
- Render graph genérico.
- Editor, assets en disco, audio, red, física. Fuera del alcance del motor en M0..M2.
- Compatibilidad binaria o de API con Bevy.

---

## 3. Diagrama de capas

```
 +-----------------------------------------------------------------------------------+
 |  Aplicación / juego / ejemplos (voxel_world, headless_world, gpu_info)             |
 +-----------------------------------------------------------------------------------+
 |  boxel (fachada): prelude, DefaultPlugins, MinimalPlugins, features                |
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
 |                                   | boxel_utils (fork de bevy_ecs 0.20.0-rc.2)          |
 +-----------------------------------+----------------------------------------------------+
 | winit 0.30.13, glam 0.34.0, binary-greedy-meshing 0.5.2, offset-allocator, slotmap ... |
 +-----------------------------------------------------------------------------------+
```

Reglas de dependencia (verificadas por el esqueleto que compila):

- `boxel_math`, `boxel_voxel`, `boxel_mesh`, `boxel_worldgen`, `boxel_gfx`, `boxel_shaders` no
  dependen del ECS. Son puros y testeables sin App.
- Solo `vendor/` depende de código derivado de Bevy. Ningún crate `bevy*` puede entrar al árbol
  (cargo-deny con nombres exactos más `tools/no_bevy.py` para cualquier nombre).
- `boxel_render` depende de `boxel_world` (consume `ChunkMeshEvent`), nunca al revés.
- El núcleo del renderer (`VoxelRenderer`) no usa el ECS; el plugin es una capa fina encima.

---

## 4. Mapa de crates

| Crate | Responsabilidad | Depende de (boxel) |
|---|---|---|
| `boxel` (raíz) | Fachada: re-exports, `prelude`, `DefaultPlugins`, `MinimalPlugins`, ejemplos | todos |
| `boxel_app` | `App`, `Plugin`, `PluginGroup`, schedules (`Main`...`Last`, fixed, `Shutdown`), `Time`, `FixedTime`, task pools, runner headless, `AppExit` | ecs, tasks |
| `boxel_log` | Subscriber de `tracing`, puente de `log`, Tracy opcional | app |
| `boxel_diagnostic` | `FrameStats`, `DiagnosticsBoard`, log periódico | app, ecs |
| `boxel_math` | re-export de glam, `Aabb`, `Frustum`, proyección reverse-Z | (glam) |
| `boxel_transform` | componente `Transform` (traslación f64) | ecs, math |
| `boxel_window` | runner winit, ventana primaria (no-send), mensajes de ventana, input | app, ecs, math |
| `boxel_camera` | `Camera`, `CameraView` (relativa a cámara), `FlyCamera` | app, transform, window |
| `boxel_gfx` | política sobre wgpu: preferencias, instancia, tiers, scoring, smoke, selección, LKG, superficie, readback, hal; ejemplo `gpu_info` | (wgpu) |
| `boxel_shaders` | WGSL embebido, composición, espejos `#[repr(C)]`, tests de naga | (wgpu, bytemuck) |
| `boxel_voxel` | coordenadas, `Face`, registry, paleta, luz, `ChunkData` | (glam) |
| `boxel_mesh` | `PackedQuad`, `ChunkMesh`, `PaddedChunk`, `Mesher` (bgm) | voxel |
| `boxel_worldgen` | trait `WorldGenerator`, `FlatGenerator`, `TerrainGenerator`, ruido | voxel |
| `boxel_world` | `ChunkMap`, ciclo de vida, scheduler, jobs, ediciones, eventos, `ChunkStore` | app, voxel, mesh, worldgen, transform, diagnostic |
| `boxel_render` | `VoxelRenderer` (High/Mid/Low), `RenderPlugin`, texturas procedurales, screenshots | gfx, shaders, world, camera, window |
| `boxel_facade_check` | solo CI: prueba que los derives resuelven `::boxel::ecs` para un usuario que depende solo de `boxel` | fachada |
| `vendor/*` (7 crates) | ECS y tareas (fork de Bevy) | - |

Features de la fachada: `default = [window, render, voxel, png, debug]`, más `trace` (spans de
sistemas, implica `debug`), `trace_tracy` (Tracy), `track_location` y `hal` (escape hatch
Vulkan). `debug` hace que los diagnósticos del ECS (conflictos B0001, errores de schedule, spans)
muestren nombres de sistemas y componentes; conviene apagarla en builds de distribución.

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
| Low | backend real; `Features::IMMEDIATES`; `max_immediate_size >= 16`; `max_vertex_buffers >= 1`; `max_texture_array_layers >= 256` |
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
| Present | `--present vsync/mailbox/immediate` | `BOXEL_PRESENT` | - | `present_mode` |
| Archivo | `--gpu-config ruta` | `BOXEL_GPU_CONFIG` | - | - |

Detalles: `WGPU_BACKEND` se parsea con `Backends::from_comma_list`; un resultado vacío se ignora
con aviso. El nombre de adaptador lo compara boxel; no se usa
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
0.20.0-rc.2 y sus dependencias internas (`boxel_ecs`, `boxel_ecs_macros`,
`boxel_ecs_macro_logic`, `boxel_macro_utils`, `boxel_ptr`, `boxel_tasks`, `boxel_utils`).
`bevy_platform` se reemplaza por un shim de unas 80 líneas propias más 3 archivos vendorizados.
Se eliminan `bevy_reflect`, hotpatching y stepping.

Reglas:

1. **Diff de librería cero.** El fork se genera con `tools/refork/refork.sh` (fuentes de
   crates.io ubicadas con `cargo metadata`, renombres mecánicos con verificación, manifiestos
   propios, parches solo de tests, backports). En cada regeneración `check_test_only.py` compara
   el árbol solo-renombrado con el parchado y falla si algún cambio cae fuera de código de test o
   de documentación. Cada archivo `.rs` modificado lleva una línea "boxel: modified by
   tools/refork" (Apache-2.0 §4(b)). Verificado: 961/961 tests de librería y 480/480 doctests
   pasan con `--locked`.
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
5. Re-fork sobre 0.20.0 final apenas salga (tarea disparada; a más tardar, criterio de entrada de
   M1; si incluye bevy#25940, se borra el backport, y el script avisa si un parche ya está
   upstream). Después, como máximo una vez al año o por un fix de seguridad; entre re-forks se
   aplican como parches los PR de Bevy etiquetados unsound/UB.
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
  recursos de GPU y ventana en orden.
- Tiempo: `Time` (delta con tope de 250 ms, elapsed, frames) en `First`; `FixedTime` a 64 Hz con
  acumulador y máximo de 8 pasos por frame; `FixedMain` = `FixedPreUpdate`, `FixedUpdate`,
  `FixedPostUpdate`. Mientras corre `FixedMain`, `Res<Time>` es la vista fija (delta = paso
  fijo), como en Bevy.
- Mensajes y paso fijo: `TimePlugin` agrega `signal_message_update_system` en `FixedPostUpdate`
  y deja la rotación de mensajes en espera hasta que corra un paso fijo. Sin esto, un lector en
  `FixedUpdate` perdía los mensajes escritos en `Update` cuando los frames iban a más del doble
  de la frecuencia fija (medido: 20 de 30 perdidos).
- Task pools: `TaskPoolPlugin` crea los tres pools globales de `boxel_tasks`. El hilo principal
  también ejecuta sistemas, así que los workers son `paralelismo - 1`: IO 10 % (1 a 2), async
  compute 40 % (1 a 8, jobs de chunks), compute el resto. Con 16 hilos: 2, 6 y 7.
- Runners: `ScheduleRunnerPlugin` (headless, `Once` o `Loop`) y el runner winit de
  `boxel_window`. El runner por defecto actualiza una vez.

---

## 7. Modelo de datos de vóxeles

- **Chunk de 32³** (`CHUNK_SHIFT = 5`). Potencia de dos: coordenadas con shifts y máscaras, LOD
  por mips 2x, edición barata (~0.1 ms de remesh contra ~0.56 ms con 62³). El mesher usa
  `Mesher<32>` con borde (34 = 32 + 2, entra en una columna `u64`).
- **Índice local** `(y << 10) | (x << 5) | z` (z más rápido): mismo orden que el `pad_linearize`
  de binary-greedy-meshing, así el gather copia filas.
- **Coordenadas:** `ChunkPos`, `BlockPos`, `LocalPos` (u16), `ChunkKey { dim, lod, pos }`,
  `Face` (orden congelado `+Y, -Y, +X, -X, +Z, -Z`), 26 vecinos en orden fijo.
- **Registry** de datos: `BlockState(u16)` denso en tiempo de ejecución (0 = aire), nunca se
  persiste crudo (M1 agrega el IdMap nombre a id). Tablas SoA: nombre, forma (`None`, `Opaque`,
  `Cutout`, `Translucent`), flags, emisión, texturas por cara. Contenido M0 congelado: aire,
  piedra, tierra, pasto, arena, grava, nieve, bedrock, tronco, hojas (cutout), agua (translúcida),
  tablones.
- **Contenedor de paleta:** `Uniform(state)` | `Packed { palette, bits 1/2/4/8, words }` |
  `Direct(u16)`. Los índices nunca cruzan palabras de 64 bits. `set` agrega a la paleta en el
  lugar mientras entra; se compacta al guardar o en reposo. Medido en un mundo de 512x256x512:
  60.6 % de los chunks son aire uniforme; la paleta reduce 128 MiB a 9 MiB (0.14 B/vóxel).
- **Luz** separada (`Uniform { sky, block }` o `Packed`), en M0 siempre cielo 15.
- **Ocupación** de 4 KiB (1 bit por vóxel): calculable en M0, mantenida incrementalmente desde M1.
- **`ChunkData`** = bloques + luz + `version`. Se comparte como `Arc<ChunkData>`; las ediciones
  usan `Arc::make_mut`. Los arrays de vóxeles nunca van inline en componentes del ECS.

---

## 8. Ciclo de vida de chunks y scheduling de jobs

Los chunks **no son entidades**. `ChunkMap` (recurso) = `SlotMap<ChunkId, ChunkSlot>` +
`HashMap<ChunkPos, ChunkId>` con foldhash `FixedState` (determinista; 6.5 ns por lookup medidos).
Los viewers sí son entidades (`Transform` + `ChunkViewer { radius, vertical_radius }`).

Estados ortogonales por slot:

- `DataState`: `Queued`, `Generating`, `Ready(Arc<ChunkData>)`, `Missing` (generación
  fallida: aire permanente).
- `MeshState`: `None`, `Queued`, `InFlight { data_version, epoch }`, `Ready { mesh, data_version,
  epoch }`, `Empty`.
- `Interest`: `DATA`, `MESH`, `PINNED`.
- `neighbors_ready`: máscara de 26 bits; `epoch`: contador que sube cuando cambia el borde de un
  vecino; `cancel`: `Arc<AtomicBool>` para cancelación cooperativa.

Cada viewer quiere un cilindro vertical: malla en el círculo `radius` (xz) y en las capas a
`vertical_radius` o menos (8 por defecto); datos un chunk más en cada dirección (círculo
`radius + 1` más los vecinos xz de cada columna con malla, que el círculo solo deja fuera en
algunas diagonales del borde); descarga más allá de otros `hysteresis` (2) chunks, en xz y en y.
El mundo no tiene alto máximo; `WorldSettings::vertical_bounds` fija uno si hace falta (con el
rango del generador y un `vertical_radius` grande reproduce el streaming de M0). Las capas fuera
de `WorldGenerator::vertical_chunks()` se crean como aire sin job: el rango es una promesa del
generador ("afuera todo es aire"), no un límite del mundo. Un vecino sin cargar no cuenta como
listo, salvo fuera de `vertical_bounds`.

Sistemas en `Update`, encadenados (`WorldSystems`):

1. **Interest:** cuando un viewer cambia de chunk o de radio, lista solo las posiciones cuyo
   nivel (malla, datos, banda de descarga, fuera) cambia: un paso corto se compara fila por fila,
   O(filas + cambios); un viewer nuevo o un salto lista su región nueva por cáscaras, la más
   cercana primero, y después la vieja. Cada posición listada se evalúa contra los viewers
   actuales dentro de `interest_budget` (1 ms por frame): crea slots (prioridad = distancia² al
   viewer más cercano), actualiza el interés o descarga (marca `cancel`, emite
   `ChunkMeshEvent::Remove`, invalida vecinos). Las prioridades de la cola quedan viejas y se
   recalculan por lotes, las más cercanas primero. El mapa reserva lugar para dos vistas: al
   saltar, la vieja sigue cargada hasta que se crea la nueva y el mapa no crece a mitad de la
   carga.
2. **Edits:** aplica `EditQueue` agrupando por chunk: `make_mut`, `set`, sube `version`, pide
   remesh del chunk y de los vecinos de cara tocados (sube su `epoch`). Prioridad 0.
3. **Dispatch:** primero vacía el canal de resultados (libera sus lugares en vuelo) y después
   saca jobs mientras `in_flight < tope`; repite mientras lleguen resultados, dentro de
   `job_budget` (2 ms por frame). Antes de sacar un job recalcula las prioridades viejas que
   podrían ir antes que él: el viewer se acercó como mucho tanto como se movió, y eso acota cada
   prioridad vieja. Así los jobs salen siempre del más cercano al más lejano. El tope por defecto
   sale de los hilos de async compute, del costo medido de un job (promedio de cada frame,
   suavizado entre frames) y del intervalo entre frames: trabajo para 1.5 frames (al menos 2 por
   hilo, como mucho 4096). Generación: captura generador, registry y clave. Malla: solo si los 26
   vecinos están `Ready` o `Missing`; captura el centro y los 26 `Arc`, la tabla de materiales,
   `data_version` y `epoch`. Atajos: centro aire uniforme, o centro opaco uniforme rodeado de
   opacos uniformes en las 6 caras, da `Empty` sin job. Descarta resultados viejos (generación
   del slotmap, `data_version`, `epoch`, `cancel`).
4. **Collect:** publica `ChunkMeshEvent::Upsert` / `Remove` de las mallas listas, hasta 512 por
   frame.

**Contrato de resultados:** todo job enviado devuelve **exactamente un** mensaje: `Generated`,
`Meshed`, `Cancelled` (vio la bandera de cancelación) o `Failed` (con el texto del pánico). El
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
un `Mesher` thread-local (~210 KiB de scratch).

---

## 9. Meshing

1. **Gather** (`Mesher::gather`): arma un `PaddedChunk` de 34³ ids de material (u16) más máscaras
   de columnas opacas y transparentes, con el centro completo y del vecino solo lo necesario
   (losas de 32x32 en caras, filas en aristas, un vóxel en esquinas). Clase: `Opaque` oculta caras
   vecinas; `Cutout` y `Translucent` son transparentes para bgm (no ocultan, y bgm omite las caras
   entre dos vóxeles transparentes del mismo material).
2. **bgm** `fast_mesh` (Mesher<32>): ~23 µs por chunk de 32³ en terreno (30 a 48x más rápido que
   `block-mesh` greedy).
3. **Conversión** a `PackedQuad` canónico: con `Face::vertices_packed` se toman min y max de los 4
   vértices; el mínimo es la esquina; `w` y `h` son la extensión en los ejes u y v canónicos de la
   cara; en caras positivas se resta 1 en el eje normal (bgm ubica el plano en `voxel + 1`).
   Validado contra un oráculo de fuerza bruta en 32 chunks reales.
4. **Buckets (8, congelados, ADR 0012):** 0 a 5 = caras opacas en orden `Face`; 6 = **cutout**
   (todas las caras de materiales cutout, como las hojas); 7 = translúcido. `ChunkMesh` guarda los
   quads contiguos por bucket y `bucket_counts[8]`.

**`PackedQuad` (8 bytes):**

```
geo: x:5 | y:5 | z:5 | (w-1):5 | (h-1):5 | face:3 | reservado:4
mat: capa:12 | ao:8 (2 bits por esquina) | cielo:4 | bloque:4 | flags:4
ejes (u, v): ±Y -> (X, Z); ±X -> (Z, Y); ±Z -> (X, Y)
```

Máximo por bucket de cara: 16 384 quads (tablero de ajedrez 3D: 32x32x16). Los buckets cutout y
translúcido juntan las seis caras: hasta 98 304 quads con un solo material por bucket, que es lo
que cubre el buffer de índices compartido. Dos materiales cutout (o translúcidos) alternados no se
tapan entre sí y llegan a 196 608; el registry de M0 tiene uno de cada (riesgo abierto, ver
`informe-m0.md`). Pasado el límite, los tres tiers dibujan solo los primeros 98 304 quads del
bucket. AO: bgm no tiene; en M0 todos los quads llevan AO abierto (0xFF). M1 decide entre AO
en la clave de merge (+36 a +65 % de quads, medido) para Mid/Low y AO en el fragment shader desde
ocupación en GPU para High.

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
  redondeada a vóxeles; los chunks guardan su origen absoluto en `i32` y el shader resta en
  enteros antes de pasar a `f32`. Precisión exacta a cualquier distancia del origen del mundo,
  sin re-basar tablas.
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
| Arena de quads | un buffer `STORAGE | VERTEX | COPY_DST` de `PackedQuad`, sub-asignado con `offset-allocator` (una asignación por chunk con los 8 buckets contiguos); 8 Mi quads = 64 MiB por defecto | todos |
| Tabla de chunks | storage `array<ChunkGpu>` (64 B: origen i32x3, `quad_base`, 8 contadores, flags, relleno) | High, Mid |
| Tabla CPU | espejo en CPU (origen, rangos, contadores) para culling en CPU | todos |
| Índice compartido | u32 `[0,1,2,2,1,3] + 4i` para 98 304 quads (2.36 MB) | High, Mid |
| Textura de bloques | `texture_2d_array` RGBA8 sRGB 16x16 con mips, procedural en M0 | todos |
| View uniform | 240 B (std140): `view_proj`, 6 planos, origen de render, cámara relativa, niebla, contadores | todos |
| Color principal | `Rgba8UnormSrgb` (`RENDER_ATTACHMENT | COPY_SRC | TEXTURE_BINDING`), luego blit a la superficie (alfa siempre 1); facilita screenshots | todos |
| Profundidad | `Depth32Float` | todos |

Layouts de bind groups (congelados): grupo 0 estático (textura + sampler), grupo 1 por frame
(view), grupo 2 geometría (arena + tabla, solo lectura, vertex) en High/Mid. Lo pesado y estático
va en el grupo 0, lo que cambia en el más alto (fast path de wgpu sin rebind, #9738).

Si el arena o la tabla se llenan, el chunk no se sube: se cuenta en `RenderInfo::dropped_chunks`,
se avisa en el log y aparece en la línea de diagnóstico `render`. El crecimiento del arena llega
en M1.

### 10.3 Pases por tier

Cada tier usa dos pipelines de vóxeles (ADR 0012): **opaco** (`fs_opaque`, sin `discard`, así se
conserva el early-Z en todo el terreno, `cull_mode: Back`) para los buckets 0 a 5, y **cutout**
(`fs_cutout`, alpha test, `cull_mode: None`) para el bucket 6, dibujado después.

**High** (Vulkan con MDI-count):

1. `clear_buffer(draw_counts)` (dos contadores: opaco y cutout).
2. Compute `cull_chunks` (64 hilos por grupo, un hilo por slot): AABB contra 6 planos; por bucket
   de cara, un test de backface a nivel chunk (un bucket +Y solo se ve si la cámara está por
   encima de su `min.y`, etc.); el bucket cutout solo usa el frustum. Por bucket visible:
   `atomicAdd` en su lista y escribe `DrawIndexedIndirect { index_count: n*6, instance_count: 1,
   first_index: 0, base_vertex: 0, first_instance: s }` y `DrawRef { chunk_slot, quad_start }` en
   la posición `s`. Cada lista tiene capacidad de hasta 65 535 draws (§5.11).
3. Pase principal: pipeline opaco + `multi_draw_indexed_indirect_count` sobre la lista opaca;
   pipeline cutout + otro sobre la lista cutout.
4. Blit a la superficie.

Hi-Z (oclusión en dos fases) queda para M2. Notas para entonces: con reverse-Z la pirámide usa
reducción **MIN** (lo lejano es 0); `Depth32Float` se lee como textura de profundidad (no
filtrable) y se copia a una cadena de mips `R32Float` en storage; solo aplica a High (GL 3.3 no
tiene compute).

**Mid** (Metal, GL >= 4.3, DX12 por ahora, Vulkan forzado):

1. Culling en CPU con los mismos tests que el shader.
2. Escribe los `DrawRef` del frame en el buffer de instancia.
3. Pipeline opaco y `draw_indexed(0..n*6, 0, i..i+1)` por bucket de cara visible; después
   pipeline cutout para los buckets cutout.
4. Blit.

**Low** (GL 3.3, GLES 3.0, Vulkan/GL forzado):

1. Culling en CPU.
2. Pipeline `vs_instanced` (opaco y luego cutout), arena como buffer de vértices por instancia
   (`Uint32x2`, stride 8), `TriangleStrip`; por bucket visible: `set_immediates(0, origen del
   chunk)` y `draw(0..4, primer_quad..primer_quad+n)`.
3. Blit.

El bucket translúcido se genera y se sube pero no se dibuja en M0 (pase translúcido con orden de
atrás hacia adelante en M1).

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
`var<push_constant>`). WESL se reevalúa en M2.

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
presenta a la frecuencia del monitor. Mailbox sigue siendo opcional (`--present mailbox`): con la
compuerta, FIFO ya no bloquea, y Mailbox dibujaría a fps ilimitados en laptops.

En GL, wgpu reproduce los comandos en el hilo que hace submit, así que los frames GL pagan el
grabado dos veces (no medido aún). El render en un hilo propio con un mundo de render (`SubApp` +
extract) es M3; la frontera de extract ya existe.

---

## 12. Persistencia (solo el trait en M0)

```rust
pub trait ChunkStore: Send + Sync + 'static {
    fn load(&self, key: ChunkKey) -> std::io::Result<Option<Vec<u8>>>;
    fn save_batch(&self, items: &[(ChunkKey, Vec<u8>)]) -> std::io::Result<()>;
    fn flush(&self) -> std::io::Result<()> { Ok(()) }
}
```

`NullChunkStore` en M0 (el mundo se regenera desde la semilla). Plan M1: redb 4.3 (Rust puro,
ACID; una transacción de escritura a la vez, así que un hilo de IO que agrupa), clave u128 =
dim | lod | Morton(x, y, z), blob con cabecera (magic, versión de formato, compresión, largo) y
payload binario propio (paleta + palabras u64), lz4_flex para guardado en caliente y zstd para
archivo. Metadatos con postcard. Medido: un mundo de 512x256x512 ocupa 1.76 MiB con lz4 y
0.81 MiB con zstd(3). Guardar solo chunks modificados (el generador es determinista). El
schedule `Shutdown` es el lugar para el `flush` final.

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
  2 s.
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
| Propiedades | paleta contra un array denso, mesher contra el oráculo de fuerza bruta (opaco, cutout, translúcido) | proptest |
| Shaders | naga: parse + validación, GLSL en el piso de cada módulo, MSL, HLSL (FXC y DXC), SPIR-V, builtins prohibidos, constantes, layouts, regla de Low | `boxel_shaders/tests/naga.rs` |
| GPU | contrato (readback, PNG, scopes, `tier_request` en adaptadores reales); selección headless y smoke por tier; misma escena en todos los tiers de cada adaptador (<= 0.1 % de píxeles distintos), escena lejana de profundidad; terreno real | `tests/gpu_*.rs`; CI con lavapipe y llvmpipe; un hilo por binario |
| Integración | `voxel_world` automatizado (`--frames`, `--timeout`, `--compare-to`, `--minimize-test`, `--resize-test`, pérdida de dispositivo), `window_smoke`, `headless_world`, test de ambigüedades de schedules | plan de verificación |
| ECS vendorizado | tests de librería y doctests de upstream con lockfile fijo; Miri en PRs del fork y semanal | `vendor/` |
| Repositorio | superficie del ECS, ningún crate `bevy*`, lockfiles sincronizados, cargo-deny, typos | `just lint`, `just deny`, CI |
| Benchmarks | criterion: paleta, meshing, worldgen (informativos en M0) | `benches/` |

---

## 15. Roadmap

| Milestone | Contenido | Criterio de salida |
|---|---|---|
| **M0 scaffold** | workspace y tooling; fork del ECS; `boxel_app`; ventana con compuerta de presentación e input; detección completa (tiers, scoring, smoke, overrides, LKG de crashes, device lost, catch_unwind); tiers High, Mid y Low con buckets opaco y cutout (High sin Hi-Z); paleta, registry, worldgen, mesher, streaming con jobs; cámara libre; `gpu_info`, `voxel_world`, `headless_world` | compila, tests, clippy y rustdoc limpios; `voxel_world` dibuja terreno en Vulkan High y en GL (Mid y Low); imágenes iguales entre tiers; la simulación no se congela con la ventana minimizada |
| **M1 mundo jugable** | criterio de entrada: re-fork sobre bevy 0.20.0 final. Pase translúcido; AO; raycast y edición de bloques (y el sistema de ediciones si se recortó); propiedades de bloque e IdMap; `ChunkStore` con redb; crecimiento del arena y presupuesto de subida; caras interiores de hojas (opcional); re-selección en caliente si se recortó; decisión `static-dxc` o DLL en Windows; timestamps de GPU; actualizar a wgpu 31 | editar y guardar un mundo; agua visible |
| **M2 GPU-driven completo** | Hi-Z en dos fases (High); cave culling por BFS (Mid/Low); pool de ocupación en GPU; `create_shader_module_trusted` con medición; pipeline cache de Vulkan; verificar DX12 High; clip control en GL (upstream); decidir WESL | caída de draws y de tiempo de GPU medida |
| **M3 escala** | LOD con mips de 32³ y anillos; listas indirectas segmentadas (más de 65 535 draws); render en hilo propio; inicialización de GPU asíncrona (respetando los error scopes por hilo); luz (cielo y bloque) | distancias de vista grandes estables |
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
| binary-greedy-meshing: crate chico, MIT, sin AO | dependencia frágil | envoltorio propio y oráculo de tests; fork in-repo en M1 si hace falta |
| Arranque lento en laptops híbridos (despertar la dGPU ~1 s) | mala primera impresión | documentado; inicialización asíncrona en M3 |
| Más de 65 535 buckets visibles en High | huecos, también cerca | aviso; segmentación en M3 |
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
