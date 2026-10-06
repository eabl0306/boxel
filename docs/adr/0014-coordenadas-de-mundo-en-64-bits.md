# ADR 0014: coordenadas de mundo en 64 bits

- Estado: aceptado
- Fecha: 2026-10-05

## Contexto

El objetivo es un mundo infinito en todas las direcciones, también hacia arriba y hacia abajo.
Las posiciones enteras del mundo (`ChunkPos`, `BlockPos`) eran `i32`. Una auditoría con pruebas
(rama `probe/far-coords`) midió que todos los ejes se rompían en el borde de `i32`: 2^31 vóxeles,
unos 2 147 millones.

- `BlockPos::from_world` saturaba: pasado el borde, la cámara seguía moviéndose pero su chunk no,
  así que el streaming dejaba de seguirla.
- `ChunkPos::origin` daba la vuelta: el chunk 2^26 recibía el terreno de -2^31, con un acantilado
  de 8 a 11 vóxeles en la costura.
- En debug, el culling en CPU (Mid y Low) y el generador `Forest` del ejemplo `stress` entraban
  en pánico por desborde.

Ya estaban bien: el render era relativo a la cámara (el shader resta enteros antes de pasar a
`f32`) y daba píxeles exactos hasta 2^30, y `Transform` ya era `f64`.

Agrandar solo los tipos no alcanzaba. El ruido del terreno muestreaba coordenadas `f64` sobre una
grilla `i32` que saturaba: con la configuración por defecto (frecuencia 1/256, 5 octavas) se
degradaba desde 2^35 vóxeles, y desde 2^39 repetía el mismo trozo cada 256 vóxeles.

## Decisión

Cuatro puntos, aprobados juntos.

### 1. Enteros de mundo en `i64` en toda la CPU

- `ChunkPos`, `BlockPos` (y por lo tanto `ChunkKey.pos`) usan `i64` en x, y, z. `LocalPos` y los
  offsets dentro de un chunk siguen chicos.
- Un solo límite de mundo: `boxel_voxel::MAX_CHUNK_COORD = 1 << 56` chunks por eje (2^61
  vóxeles, unos 2.3e18). `MAX_BLOCK_COORD = MAX_CHUNK_COORD << 5` se deriva de él. A lod k el
  límite es `MAX_CHUNK_COORD >> k` (los mismos vóxeles). 2^56 deja margen para que nada desborde
  `i64`: los orígenes (`chunk << (5 + lod)`) llegan como mucho a 2^61, y cualquier offset de
  vecino, radio o histéresis (hasta 4096 chunks) y cualquier diferencia entre dos posiciones
  entran en `i64`.
- `ChunkPos::origin` es exacto para los chunks en `-2^58..2^58` (todo chunk que contiene un
  bloque `i64`) y comprueba en debug que el shift no pierde bits.
- `BlockPos::from_world` hace `floor` y pasa a `i64`: exacto para todo punto finito dentro de
  `i64`; afuera satura y NaN da 0.
- El mundo sujeta a sus viewers al límite (`Viewer::new` en el interés de `boxel_world`,
  `ChunkPos::clamp_to_world`): un viewer más allá carga los chunks del borde. Antes el centro se
  sujetaba a 2^28 chunks para que todo entrara en `i32`. Las posiciones armadas a mano no se
  comprueban, salvo con `debug_assert`.
- Las distancias al cuadrado (`ChunkPos::distance_sq`, `distance_sq_xz`) se calculan con
  `abs_diff` en `u128`: exactas dentro del límite, saturan solo muy afuera.
- El planificador de interés acepta cualquier posición: las diferencias saturan
  (`saturating_sub`, `saturating_abs`) y una posición saturada queda fuera de toda tabla.
  `WorldSettings::vertical_bounds` y `WorldGenerator::vertical_chunks` pasan a
  `RangeInclusive<i64>`. Los límites verticales no se sujetan, así que un rango vacío con
  extremos lejanos se detecta antes de restar (`Planner::capacity_hint`).

### 2. Ruido desde enteros

- `TerrainGenerator::height_at` y el ruido toman columnas `i64`. La frecuencia es una fracción
  exacta, `noise::Frequency` (`num / den` en términos mínimos: `Frequency::one_in(256)` por
  defecto, `Frequency::new(num, den)`), y la lacunaridad es un entero: `Fbm { frequency,
  octaves, lacunarity: u32, gain }`.
- Por octava, la celda es `floor(x * num / den)` y la posición dentro de ella
  `((x * num) mod den) / den`. Se calcula en `i64` cuando el producto entra (shift y máscara si
  `den` es potencia de 2) y en `i128` si no. La celda es un entero exacto (`i64`, o `i128` en
  octavas más finas que un vóxel muy lejos); solo la posición en [0, 1) pasa a `f64`. Una
  coordenada absoluta nunca es `f64`.
- El fbm se corta antes de la primera octava cuyo numerador (`num * lacunarity^k`) desborda
  `u64`, es decir, con más de 2^32 celdas por vóxel. Con lacunaridad 2 quedan al menos 33
  octavas (64 con una frecuencia `1 / n`).
- `noise::hash2(seed, x: i64, z: i64)`: la clave son los 32 bits bajos de x y z
  (`x << 32 | z`) y lo que sobra arriba de `i32` (`e`) identifica el bloque de 2^32 x 2^32
  celdas. Dentro de `i32` (`e = 0`) el hash es `SplitMix64(SplitMix64(seed) ^ key)`, el mismo de
  antes bit a bit. Afuera, la etiqueta `mix(e)` entra después de hashear la clave:
  `SplitMix64(SplitMix64(SplitMix64(seed) ^ key) ^ mix(e))`. `mix` es biyectiva con
  `mix(0) = 0`, así que cada bloque tiene una etiqueta distinta y ninguno es una copia corrida o
  reetiquetada de otro. Las celdas más allá de `i64` (solo octavas más finas que un vóxel, muy
  lejos) suman sus bits altos a la etiqueta con un hash de pares de 64 bits: ahí sí se repiten
  bloques enteros de 2^64 x 2^64 celdas, sin efecto visible (es ruido blanco de amplitud ínfima).
- Resultado: dentro del rango `i32` de antes, `hash2` da siempre los mismos valores. Las alturas
  y los chunks son iguales bit a bit con la configuración por defecto, y con cualquier
  denominador potencia de 2 en todo lugar donde el ruido de antes era exacto (celdas dentro de
  `i32` y `x * num` exacto en `f64`). Con otros denominadores difieren a lo sumo en el redondeo
  del ruido de antes. Fuera de ese rango el terreno es igual de variado a cualquier distancia,
  sin aplanarse ni repetirse.

### 3. La GPU sigue en 32 bits

- El layout de la GPU no cambia. `ChunkGpu.origin` (`i32x3`), `ViewUniform.render_origin` y el
  origen por draw de Low (`ChunkImmediate`) llevan los 32 bits bajos de los valores `i64`
  (`boxel_math::wrap_to_i32`, `CameraView::gpu_render_origin`).
- Los shaders solo usan `chunk_origin - render_origin`, en `i32`. En WGSL la aritmética entera
  da la vuelta, así que esa resta es la diferencia real módulo 2^32. Si la distancia real es
  menor que 2^31 vóxeles, el único valor `i32` con ese resto es la diferencia exacta, sin
  importar lo grandes que sean los dos orígenes. Después pasa a `f32` un número chico: los
  chunks que se ven están a unos miles de vóxeles.
- En la CPU, `CameraView.render_origin`, `ViewParams.render_origin` y el origen de
  `VoxelRenderer::upsert_chunk` son `I64Vec3`. El culling en CPU (Mid, Low) resta en `i64` antes
  de pasar a `f32`.
- Chunks cargados lejos de la cámara. Después de un teletransporte el área vieja sigue cargada
  unos frames (el mundo la quita con presupuesto de tiempo), y con la cámara más allá del límite
  del mundo los chunks cargados quedan en el borde. Si un chunk está a 2^31 vóxeles o más, su
  resta da la vuelta y lo pondría cerca de la cámara (con un salto de exactamente 2^32, justo
  encima del terreno nuevo). Por eso la tabla de chunks de `boxel_render` mantiene una ventana:
  solo se dibujan los chunks a menos de `GPU_WINDOW` = 2^30 vóxeles del centro en cada eje.
  `ChunkTable::follow` corre en cada frame, en `VoxelRenderer::render`, antes de cualquier cull,
  y mueve el centro al origen de render cuando este se aleja más de `GPU_WINDOW_SLACK` = 2^20
  vóxeles. Un chunk dentro de la ventana está entonces a menos de 2^30 + 2^20 < 2^31 de la
  cámara (comprobado en compilación) y su resta es exacta. Vale para los tres tiers:
  - High: la tabla de GPU marca `FLAG_LIVE` solo en los chunks de la ventana
    (`ChunkTable::gpu_entry`, también al subir un chunk) y `cull.wgsl` salta los demás. Cuando la
    ventana se mueve se sube la tabla entera otra vez (`ChunkTable::gpu_entries`).
  - Mid y Low: `CpuCull::run` salta los chunks fuera de la ventana a cualquier distancia de vista,
    incluso infinita, porque también dibujan con los orígenes de 32 bits.
  - Los chunks de afuera siguen cargados hasta que el mundo los quita, y se vuelven a dibujar si
    la cámara regresa.

### 4. `Transform` sigue en `f64`, sin origen flotante

No se re-basan posiciones. El render ya es relativo a la cámara y los enteros son exactos hasta
el límite del mundo, así que el límite práctico pasa a ser el paso del `f64` de `Transform` (ver
la tabla de Consecuencias).

## Consecuencias

Límites, de cerca a lejos:

| Distancia al origen (vóxeles) | Qué pasa |
|---|---|
| 2^31 (unos 2.1e9) | antes se rompía todo; ahora nada cambia |
| unos 4.4e12 (2^42) | el `f64` de `Transform` avanza de a 1 mm |
| unos 1.4e14 (2^47) | avanza de a 3 cm |
| unos 4.5e15 (2^52) | avanza de a un vóxel: la cámara ya no se mueve suave |
| 2^61 (unos 2.3e18) | límite del mundo: los viewers se sujetan acá, los enteros siguen exactos |

Con vóxeles de 1 m, el límite del mundo está a unos 240 años luz del origen.

Costo:

- `ChunkPos` pasa de 12 a 24 bytes y `ChunkKey` de 16 a 32. Los mapas de chunks y las colas del
  planificador crecen en la misma proporción.
- Ruido, medido con criterion (`boxel_worldgen`, bench `terrain`, contra la base de antes): cerca
  del origen, igual (`surface_chunk` 46.5 a 46.1 µs, `height_at_32x32` 63.5 a 62.3 µs,
  `air_chunk_in_band` 11.24 a 11.25 µs). A 2^35 vóxeles cada muestra es un 6 % más lenta, y
  `height_at_32x32_far` (columnas en 2^40) tarda 76.6 µs, 1.23 veces lo de cerca, por la etiqueta
  de bloque que cada octava calcula afuera de `i32`. Se midió antes de mover la etiqueta después
  del hash (0090801), que agrega un mix por esquina lejana; cerca del origen ese cambio no hace
  nada.
- GPU: nada extra por frame. Cuando la ventana se mueve (una vez por salto largo, o cada 2^20
  vóxeles de viaje) se sube la tabla entera: 64 B por slot, 2 MiB con los 32 768 slots por
  defecto.

Cambios de API que rompen:

- `ChunkPos` y `BlockPos` tienen campos `i64`. `ChunkPos::distance_sq` y `distance_sq_xz`
  devuelven `u128` (antes `i64`). Nuevos: `MAX_CHUNK_COORD`, `MAX_BLOCK_COORD`,
  `ChunkPos::clamp_to_world` y las conversiones con `I64Vec3` (`from_i64vec3`, `as_i64vec3`).
- `WorldGenerator::vertical_chunks` y `WorldSettings::vertical_bounds` son
  `RangeInclusive<i64>`.
- `TerrainGenerator::height_at(x: i64, z: i64)` y `TerrainSettings.frequency: Frequency` (antes
  `f64`).
- `noise`: `value_noise_2d(seed, x: i64, z: i64, Frequency)`, `fbm_2d(seed, x, z, Fbm)` (antes
  coordenadas `f64` y parámetros sueltos) y `hash2(seed, x: i64, z: i64)`.
- `CameraView.render_origin`, `ViewParams.render_origin` y el `origin` de
  `VoxelRenderer::upsert_chunk` son `I64Vec3`. Nuevos: `boxel_math::wrap_to_i32`,
  `CameraView::gpu_render_origin` e `I64Vec3` en `boxel_math`.
- Un `ChunkStore` tiene que guardar los 64 bits de cada eje de la clave.
- Ejemplos: `headless_world --jump` toma chunks `i64` y sale con código 5 si el mundo se asienta
  lejos del viewer; el `--camera` de `voxel_world` y `stress` se comprueba contra el límite.

Tests que lo vigilan:

- `boxel_voxel`, `tests/far_coords.rs`: idas y vueltas en ±2^31, 2^40 y el límite, chunks del
  otro lado del borde de antes que ya no se pisan, `from_world` exacto lejos, offsets y
  distancias en el límite, `clamp_to_world` y una proptest en todo el mundo.
- `boxel_camera`, `tests/far_coords.rs`: origen de render y ojo exactos lejos, posiciones
  relativas iguales a las del origen y resta envuelta exacta cruzando el borde.
- `boxel_world`: la proptest del planificador (`incremental_interest_matches_from_scratch`)
  camina en x/z y en y cerca del origen, en el borde de antes, en -2^40 y contra el límite;
  `streaming_follows_the_viewer_far_from_the_origin`, `streaming_follows_the_viewer_far_in_y` y
  `bounds_far_outside_the_world_limit_hold_nothing`.
- `boxel_worldgen`: `tests/far_coords.rs` (alturas y chunks dentro de `i32` iguales a los
  grabados antes del cambio, sin costura en ±2^31 ni donde las celdas salen de `i32`,
  estadísticas del terreno iguales a las del origen a cualquier distancia, alturas y chunks
  lejanos grabados); `tests/noise_oracle.rs` (una copia privada del ruido de antes: bit a bit con
  denominadores potencia de 2, dentro de su redondeo con 1/7, 19/10 y 3/1000);
  `src/noise/tests.rs` (`hash2_is_unchanged_inside_i32`,
  `far_tiles_are_not_relabeled_copies_of_the_origin_tile`,
  `fbm_stops_before_the_frequency_overflows`, entre otros).
- `boxel_render`: en `chunk_table`, `only_chunks_near_the_camera_are_live_on_the_gpu` y
  `live_chunks_have_exact_wrapped_differences`; en el culling en CPU,
  `cpu_cull_is_exact_across_the_former_i32_edge` y
  `chunks_outside_the_gpu_window_are_culled_at_any_view_distance`; y `tests/gpu_terrain.rs`: cada
  tier dibuja el mismo terreno corrido a `FAR_SHIFTS` (cruzando ±2^31 en x, z e y, y más allá de
  2^40) con los mismos píxeles, y con la cámara teletransportada 2^32 vóxeles y los chunks
  todavía cargados no dibuja nada. Sin la ventana, High falla ahí (29.6 % de los píxeles
  dibujados).
- `boxel_debug`: `far_positions_show_their_true_chunk`.

Lo que falta:

- LOD (M3): todavía no hay orígenes de LOD. Un chunk de lod k empieza en `pos << (5 + k)`; su
  cálculo tiene que comprobar los bits como `ChunkPos::origin`, y los anillos lejanos tienen que
  caber en la ventana de la GPU (2^30 vóxeles alcanza de sobra).
- Física, raycast y edición (M1 en adelante): tienen que trabajar con posiciones de bloque `i64`
  o relativas (a la entidad, a un chunk), nunca con `f32` absolutos. Las entidades siguen en
  `f64`, con los límites de la tabla.

## Alternativas descartadas

- Origen flotante (re-basar el mundo cuando la cámara se aleja): no arregla nada de lo que estaba
  roto, porque el render ya era relativo a la cámara y lo roto eran los enteros y el ruido.
  Re-basar cada entidad y cada sistema que guarda posiciones es fácil de hacer mal, y un servidor
  multijugador no puede tener un solo origen.
- `Transform` como celda `i64` más un offset local: más precisión en todo el mundo, pero cada
  sistema de gameplay tendría que manejar posiciones de dos partes. Es el paso siguiente si
  alguna vez hace falta ir más allá de 2^42.
- `i128` o más: con vóxeles de 1 m, `i64` llega a unos 1 000 años luz. Duplicaría otra vez la
  memoria de cada posición sin ningún uso.
- `i64` en la GPU: WGSL no tiene enteros de 64 bits portables, y no hace falta: la resta en 32
  bits ya es exacta para todo lo que se dibuja.
