# ADR 0015: luz de vóxeles

- Estado: aceptado (fases P0 y P1 hechas; P2 en adelante, pendientes)
- Fecha: 2026-10-06

## Contexto

El dueño pidió primero luz de vóxeles al estilo Minecraft: luz de cielo más luz de bloque de
color (RGB), luz suave, oclusión ambiental (AO) y cuevas oscuras. Las sombras del sol con shadow
maps vienen después. Día y noche, sí.

Lo que había antes:

- `ChunkData.light` (`LightStorage`) valía siempre `Uniform { sky: 15, block: 0 }` y solo lo
  leían tests. `PackedQuad` reservaba `cielo:4 | bloque:4 | flags:4` en `mat`, sin uso.
- El AO no se calculaba nunca: `PackedQuad::new` pone 0xFF y `with_ao` no tiene llamadas fuera de
  los tests. Medido antes: AO en la clave del merge cuesta +36 a +65 % de quads.
- La ocupación de 4 KiB por chunk "mantenida desde M1" (ADR 0005) no existía:
  `opaque_occupancy` la recalculaba en cada llamada.
- El mundo es cúbico e infinito hacia arriba y hacia abajo (ADR 0014). Lo que hay encima de un
  vóxel puede no estar cargado: a y = -1000 la superficie no está en memoria.

Se compararon tres propuestas (`.claude/plans/light-plan.md`):

- **A.** Luz horneada en los quads con una paleta de luz por chunk, calculada dentro del job de
  malla. 39 de 50.
- **B.** Un volumen de luz de 34³ por chunk, muestreado por píxel. 32 de 50: AO sin resolver,
  20 a 80 KB por chunk iluminado, y Low no puede recibir una segunda textura.
- **C.** Bricks de 8³ dispersos muestreados por píxel, con el techo de cielo calculado bajando por
  cada columna. 36 de 50: el diseño más complejo, hasta 8 lecturas dependientes por píxel, y el
  mismo problema de Low.

Comprobaciones contra el código que decidieron detalles:

- Low no puede tomar una segunda textura: wgpu-hal 30.0.1 activa `SHADER_BINDING_LAYOUT` cuando
  el contexto "soporta compute" (`gles/adapter.rs:644`) y después omite el remapeo de unidades de
  textura (`gles/device.rs:544`). Una segunda textura caería en la unidad 0. Sí funciona un
  segundo vertex buffer por instancia: sin layouts, GL reporta 16 vertex buffers
  (`gles/adapter.rs:800-806`) y la emulación de `first_instance` vuelve a enlazar cada buffer
  por instancia (`gles/command.rs:134-160`).
- El método de A para el techo de cielo (`sky_floor` con un mínimo por generador) es incorrecto en
  el overworld: el techo de las cuevas vale 0 por defecto, así que cualquier tipo de cueva puede
  abrir el vóxel de arriba de una columna, y las entradas (64 de profundidad) pueden dar a cavernas
  más abajo. El método exacto es el de C: bajar por la columna.
- El paso fijo es de 64 Hz (`FixedTime::from_hz(64.0)`).
- El job de malla ya clona el centro y los 26 vecinos y solo corre cuando están todos: la luz
  entra en ese job sin estado nuevo por chunk.

## Decisión

Propuesta A corregida y extendida: luz horneada por esquina de quad con una paleta de luz por
chunk, calculada dentro del job de malla por una caja exacta. De C se toma la caminata por columna
para el techo de cielo; de B, la caché de máscaras como requisito, la prueba exacta del conjunto a
reiluminar y los techos editados persistidos.

### 1. La luz es una función pura

La luz de un vóxel depende solo de los bloques y de un techo de cielo `T(x, z)` por columna. No se
guarda en `ChunkData`, no se persiste y nunca se propaga de un chunk a otro.

Reglas, todas con enteros:

- Opacidad 15 es opaco: la luz no entra. 0 es transparente (aire, vidrio). 1 a 14 es parcial:
  hojas y agua valen 1.
- Entrar en un vóxel cuesta `max(1, opacidad)` niveles.
- Cielo directo: un vóxel con `y > T(x, z)` tiene cielo 15. Cualquier otro vóxel no opaco toma,
  por canal, el máximo sobre sus 6 vecinos de `nivel(vecino) - costo`.
- Un emisor toma su propia emisión aunque sea opaco.
- Los canales (cielo, rojo, verde, azul) se mezclan con el máximo por canal.

El punto fijo es único y no depende del orden, de la cantidad de hilos ni de la posición en el
mundo.

### 2. Datos de bloque (P0, hecho)

- `BlockDef { emission: [u8; 3], light_opacity: u8 }` reemplaza a `light_emission: u8`.
  Validación (`RegistryError::InvalidLight`): cada canal y la opacidad valen como mucho 15, y la
  forma `Opaque` exige opacidad 15. `StateFlags::EMISSIVE` es "algún canal mayor que 0".
- Contenido de fábrica: bloques opacos 15, hojas y agua 1, aire 0, ningún emisor. Las lámparas de
  color llegan en P6. `BlockRegistryBuilder::with_default_blocks` permite agregar bloques
  después de los de fábrica.
- `LightValue(u16)`: cielo en los bits 12 a 15, rojo 8 a 11, verde 4 a 7, azul 0 a 3. La emisión
  del registry usa el mismo formato con cielo 0.
- `LightTables::from_registry`: tablas SoA de opacidad, emisión y clase (opaco, parcial, emite),
  más una huella del contenido. Solo luz: la colisión no está ahí.

### 3. Máscaras de luz de `ChunkData` (P0, hecho)

`LightMasks` es una vista con el layout de `Occupancy` (bit z de la fila `y * 32 + x`): `opaque`
(opacidad de luz 15; no es la forma de render, un bloque `Cutout` puede frenar la luz), `partial`
(1 a 14) y la lista de emisores con su emisión. La entrega `ChunkData::light_masks`:

- Un chunk uniforme sin emisión lee palabras estáticas. Uno de 1 bit sin emisión lee sus propias
  palabras empaquetadas, tal cual o invertidas según el orden de la paleta. Ninguno construye nada
  ni ocupa memoria, y casi todo chunk bajo tierra es así (las cuevas son de 1 bit).
- Los demás (`Packed` de 2 bits o más, `Direct`, y todo chunk con un emisor) construyen
  `OwnedLightMasks` la primera vez y la guardan en un `OnceLock` privado: 4 KiB, 4 KiB más solo si
  algún vóxel es parcial, más los emisores. La comparten todos los lectores del snapshot: `clone`
  empieza vacío (así que `Arc::make_mut` con el snapshot compartido también), la igualdad la
  ignora, y `set` y `blocks_mut` la tiran. Al construir, los `Packed` llenan de una vez las
  palabras con un solo índice (corridas de aire o piedra) y decodifican el resto.
- No hay máscara sólida: nada la leía, y la física propia lee los bloques con su propia tabla de
  colisión.
- `ChunkData.blocks` pasa a privado (`blocks()`, `blocks_mut()`, `from_paletted()`, `compact()`)
  para que ningún cambio se saltee la caché.
- `ChunkData::may_emit` recorre solo la paleta: puede decir que sí sin emisores, pero nunca dice
  que no si hay uno.
- Se borraron `ChunkData.light` y `LightStorage`.

### 4. Techo de cielo por columna (P1, hecho; el mundo lo usa en P3)

`T(x, z)` es el vóxel más alto con opacidad mayor que 0 en el contenido del mundo (generador más
ediciones), nunca en "lo que está cargado". Por eso el streaming nunca cambia la luz: un chunk que
llega desde arriba o desde abajo no reilumina nada.

`SkyTile` guarda los 1024 techos de una columna de chunks (8 KiB), más `min`, `max`, `floor`,
`version` y `edited`. `sky_tile::walk` lo calcula:

- Empieza en `S`, el vóxel más alto que puede tener contenido en la columna de chunks: el tope del
  rango vertical del generador o el bloqueador editado más alto de la columna, el que sea mayor.
- Baja capa por capa. Cada capa sale del snapshot cargado si lo hay, de una generación privada si
  no, o es aire (chunk `Missing` o fuera del rango del generador).
- Termina cuando todas las columnas encontraron un bloqueador.
- **Piso** (`sky_walk_limit`, 1024 por defecto): una columna todavía abierta en el piso recibe
  `T = piso`, como si hubiera un bloqueador ahí. El piso es del mundo, no del tile: el tope del
  generador menos el límite (`walk_floor`), y las ediciones no lo mueven. Un bloque puesto muy
  arriba solo sube el comienzo de su tile, así que cambia su columna y ninguna otra, y una edición
  aplicada al tile (`raise`, `rescan_column`) da los techos de una caminata nueva. Es determinista
  y depende solo del contenido. Solo lo notan los pozos naturales más hondos que el límite.
- **Vacío**: debajo del rango del generador todo es aire. Una columna que llega abierta hasta ahí
  recibe `T = i64::MIN` (abierta para siempre). Debajo del límite del mundo también es vacío.
- **Generador sin tope**: `GeneratorTop::of` distingue un rango vacío, un tope y un rango sin tope
  (el de `WorldGenerator` por defecto, que no promete nada). Sin tope no hay dónde empezar: P3
  rechaza ese generador o corre sin techos de cielo, nunca camina desde el límite del mundo (todo
  quedaría oscuro).

Así, cada techo es `i64::MIN` o está en el piso o arriba. Una caja cuyo tope está
en o debajo de todos los techos de sus 9 columnas no tiene cielo directo. Si ninguna columna del
mundo llega al vacío dentro del límite (el overworld, o generadores acotados con columnas
cerradas), eso vale para toda caja debajo del piso, sin pedir ningún tile.

Ediciones (P3): poner un bloqueador encima de `T` sube `T` (`SkyTile::raise`). Quitar el bloque de
`T` busca el siguiente hacia abajo en los datos cargados (`SkyTile::rescan_column`); si la
búsqueda sale de lo cargado, la columna vuelve a `Pending` y corre una caminata. Queda un caso
para P3: en un mundo cuyas columnas llegan al vacío (uno de aire), una edición debajo del piso hace
que una caminata nueva cierre en el piso todas las columnas abiertas del tile, y `raise` solo la
suya.

### 5. Caja exacta por chunk (P1, hecho; corre dentro del job de malla en P3)

`boxel_light::compute` calcula la luz del chunk acolchado (local -1 a 32, lo que lee el mesher):

- **Por qué alcanza una caja.** La luz baja al menos 1 por vóxel y empieza en 15, así que toda luz
  de 1 o más en el chunk acolchado viene de una fuente a 14 vóxeles o menos, por un camino que no
  se aleja más de 14. La caja es el chunk más 16 vóxeles por lado (64³, una fila en z es un
  `u64`): contiene toda fuente y todo camino así. La inundación dentro de la caja da la solución
  global bit a bit, y la caja cae dentro de los 27 chunks que el job de malla ya tiene.
- **Caminos rápidos**, con los emisores dentro de la caja y los límites de los tiles: sin
  emisores y con la caja debajo de todos los techos, oscuridad uniforme (`FastDark`, cubre toda
  cueva profunda); sin emisores y con el chunk acolchado encima de todos los techos, cielo 15
  uniforme (`FastOpen`). Los emisores se buscan solo en los chunks cuya paleta puede emitir, así
  que una lámpara que ya no está pero sigue en la paleta, o una fuera de la caja, no los impide.
- **Camino completo**: filas opacas desde las 27 `LightMasks`; filas de cielo directo desde los 9
  tiles; semillas en los vóxeles abiertos no directos vecinos de uno directo, con `15 - costo`; una
  inundación por canal (cielo, rojo, verde, azul) con una cola por niveles, del más alto al más
  bajo. En cada canal un vóxel se procesa una sola vez, ya con su valor final. Los vóxeles de
  nivel 1 se escriben pero no se encolan. Las filas escritas se limpian con un bitset (sin borrar
  512 KiB). Las opacidades parciales mayores que 1 piden la opacidad del vóxel solo si el registry
  tiene alguna.
- **Coordenadas**: dentro de la caja todo es chico. La única comparación absoluta (techos contra
  la caja) es una resta en `i64` relativa a la caja, saturada: el resultado es el mismo en
  cualquier posición hasta el límite del mundo (ADR 0014).
- Salidas: `PaddedLight` (uniforme o 34³ `u16`, en el orden de `PaddedChunk::index`) y
  `ChunkLight` (la luz del chunk para gameplay: uniforme 0 B, solo cielo 16 KiB o completa
  64 KiB), que el mundo guarda dentro de `light_cache_radius` (8 chunks).
- `LightScratch` ocupa unos 0.7 MiB por hilo.

### 6. Malla: `PackedQuad` v2 con paleta de luz (P2)

- El quad sigue en 8 bytes. `geo` usa sus 4 bits reservados como flags (bits 28 a 31). `mat`
  pasa a `capa:12 | ao:8 | índice de paleta de luz:12`. Desaparecen los campos cielo, bloque y
  flags de `mat` (`MAT_SKY_SHIFT`, `MAT_BLOCK_SHIFT`, `MAT_FLAGS_SHIFT`); entran
  `GEO_FLAGS_SHIFT = 28`, `MAT_LIGHT_SHIFT = 20` y `MAT_LIGHT_MASK = 0xFFF`.
- Paleta por chunk de hasta 4096 entradas de 8 bytes (`[u32; 2]`: las 4 esquinas en
  `LightValue`, en el orden de esquinas del AO y de `common.wgsl`), en orden determinista de
  primera aparición. Si se pasa de 4096, los canales de bloque del chunk se recuantizan a 3 bits y
  se cuenta en las estadísticas.
- Luz de esquina: los 4 vóxeles de la capa de enfrente alrededor del vértice, sin los opacos (y
  sin el de la esquina cuando los dos laterales son opacos), promedio redondeado por canal. Depende
  solo del vértice, así que dos caras coplanares visibles que comparten el vértice ven lo mismo: un
  quad fusionado es exacto píxel a píxel.
- AO: la regla de esquina de siempre sobre los 3 vecinos de enfrente (los dos laterales opacos dan
  0; si no, 3 menos los opacos). Solo cuentan los bloques opacos: las hojas no dan AO.
- **AO omitido en caras oscuras**: una cara con sus 4 esquinas en 0 lleva AO "abierto" en su
  clave. Casi toda la geometría de cuevas está a oscuras, así que se fusiona como hoy. Es
  invisible con el piso de ambiente por defecto. Política `AoPolicy::BakedLitOnly` por defecto
  (`Off` y `Baked` también existen); hay que revisarla cuando lleguen luces en la mano.
- Clave del merge: (material, byte de AO, índice de paleta), llevada a un id `u32` denso. Hace
  falta un fork en el repo de `binary-greedy-meshing` 0.5.2 (aviso MIT incluido) que fusione por
  clave de cara en lugar del `u16` del vóxel. El crate queda como dependencia de desarrollo para
  la prueba de regresión: con `AoPolicy::Off` y sin luz, los quads son idénticos byte a byte.

### 7. GPU (P4)

- La paleta vive en el arena de quads, justo después de los 8 buckets del chunk:
  `light_base = quad_base + total_quads`, en un relleno libre de `ChunkGpu` (offset 52; sigue en
  64 bytes). High y Mid la leen del mismo storage buffer: ningún binding nuevo.
- Low recibe la paleta expandida por quad en un segundo vertex buffer por instancia
  (`Uint32x2` en `@location(1)`), paralelo al arena con los mismos offsets: un atributo, no un
  binding, así que la regla de ADR 0011 se mantiene. `boxel_gfx::tier::floors::VERTEX_BUFFERS`
  pasa de 1 a 2. Low paga 8 bytes más por quad (16 en total).
- `ViewUniform` pasa de 240 a 336 bytes (solo `vec4`, std140): `sky_light` (color del cielo por
  luz del día, y piso de ambiente), `light_params` (fuerza del AO, 0.55 por defecto, igual al
  `mix(0.45, 1, ao)` de hoy; ganancia de luz de bloque) y `light_curve` (16 niveles).
- Fragment: interpolación bilineal por canal de las 4 esquinas en (s, t) del quad;
  `luz = max(curva(cielo) * sky_light.rgb, curva(rgb) * ganancia, ambiente)`;
  `color = albedo * sombreado de cara * mix(1 - fuerza_ao, 1, ao / 3) * luz`, después niebla. Con
  cielo 15 al mediodía y AO abierto da la imagen de hoy. Igual en todos los tiers.

### 8. Día y noche (P5)

Solo un uniform: sin remallado y sin subidas. `TimeOfDay` avanza en `FixedUpdate` (64 Hz; un día
de 20 minutos son 76 800 ticks). La luz de bloque se ve igual al mediodía y a medianoche.

### 9. Más adelante (P8)

AO desde ocupación en GPU en High y Mid si el gate G1 o las mediciones lo piden; cascadas de
sombras del sol en High (profundidad con solo `geo`, el cielo horneado como máscara de
exterior); luces dinámicas en el shader; vidrio teñido.

### Fases

P0 (datos de bloque, máscaras, este ADR) y P1 (`boxel_light`) están hechas. Siguen P2 (mesher),
P3 (integración en `boxel_world`: `SkyMap`, jobs de caminata, luz en `start_mesh`, ediciones con
lotes atómicos), P4 (render), P5 (día y noche), P6 (contenido: lámparas de color y lava
provisoria), P7 (medir con la máquina libre y decidir) y P8.

### Valores por defecto (decisiones abiertas del dueño)

Se usan las recomendaciones del plan mientras el dueño no decida otra cosa:

- AO horneado en la clave del merge en todos los tiers, omitido en caras sin luz.
- Piso de ambiente en cuevas 0.02 (casi negro); cielo de noche con intensidad 0.2 y tinte azul;
  día de 20 minutos.
- Límite de la caminata: 1024 vóxeles debajo del tope del generador (el piso es el mismo en todo
  el mundo; las ediciones no lo mueven).
- Hojas y agua con opacidad 1; las hojas no dan AO; mezcla RGB por máximo por canal; esquinas
  redondeadas a 4 bits; un bloqueador sombrea todo lo de abajo para siempre, como en Minecraft.
- Primeros emisores: lámparas de cubo completo de colores y una lava provisoria; antorchas cuando
  haya formas que no sean cubos.
- Curva de brillo `f / (4 - 3f)` al estilo Minecraft, con ajuste de gamma.
- Radio de la caché de luz para gameplay: 8 chunks.
- La luz va antes que el culling de cuevas.

## Consecuencias

Medido con criterion (release) el 2026-10-06, después de la revisión de P0 y P1 y de que
`tools/wait-idle.sh 60` dio la máquina por libre. La máquina estaba compartida con otros builds
(load average de 6 a 12 en 16 hilos), así que son números indicativos: se repiten en P7 con la
máquina libre. Una primera corrida salió ruidosa (`box_surface` 99 µs contra `box_far` 57 µs, el
mismo trabajo) y se descartó. Mediana de cada bench:

| Bench | Medido | Objetivo |
|---|---|---|
| `masks/cave_1bit` (vista de las palabras de 1 bit, sin construir nada) | 0.006 µs | 2 µs o menos |
| `masks/surface_packed` | 16.3 µs | 25 µs o menos (`opaque_occupancy_terrain`: 42.6 µs) |
| `light/box_cave_dark` (camino rápido) | 0.04 µs | 5 µs o menos |
| `light/box_surface` (overworld real, árboles y una entrada) | 56.5 µs | 150 µs o menos |
| `light/box_surface_cold_masks` (con las máscaras por construir) | 173 µs | sin objetivo |
| `light/box_cave_lamps_rgb` (cueva con 19 % de aire, lámparas en el 1.5 % del piso, 3 colores) | 326 µs | 500 µs o menos |
| `light/box_far` (la caja de superficie en x = z = 2^40) | 52.5 µs (0.93x) | 1.15x o menos |
| `sky_tile/walk_generate` | 128 µs | 400 µs o menos |
| `sky_tile/walk_loaded` (máscaras sin construir; las capas de 1 bit no construyen nada) | 18 µs | 60 µs o menos |

Los benches de caja comprueban que toman el camino de su objetivo (camino completo con pops de
cielo en la superficie, de bloque en las lámparas) y que la caja de superficie tiene árboles.

- La caja de superficie hace 2760 pops de cielo; la de lámparas, unos 36 000 pops de bloque
  (3 canales). El cielo por dilatación de bitsets (el método de C) no hizo falta.
- Memoria: 8 KiB por `SkyTile` (unos 7 MiB a radio 16, 27 MiB a radio 32); unos 0.7 MiB de
  scratch por hilo. Máscaras: nada en los chunks uniformes y de 1 bit sin emisión, que son casi
  todos bajo tierra; 4 a 8 KiB más los emisores en los demás. La primera versión guardaba 12 KiB
  por chunk no uniforme: a y = -1000 eran unos 155 MiB a radio 16 y 630 MiB a radio 32, encima de
  66 y 244 MiB de vóxeles. En superficie (y = 80) el techo es 8 KiB por chunk con malla: como mucho
  52 MiB a radio 16 y 210 MiB a radio 32, menos en la práctica.
- Hasta P2 y P4 no cambia nada en pantalla: el mesher y el renderer no se tocaron y los quads
  siguen en v1.
- Riesgos abiertos: el AO en la clave del merge infla los quads (gate G1 en P3); Low paga 8 bytes
  más por quad (16 en total); una paleta con más de 4096 entradas pierde precisión en los canales
  de bloque; las ediciones de luz remallan hasta 8 chunks.

Cambios de API que rompen:

- `BlockDef.light_emission` desaparece: `emission: [u8; 3]` y `light_opacity: u8`.
  `BlockRegistry::light_emission` pasa a `emission` (devuelve `LightValue`) y `light_opacity`.
  Nuevos: `RegistryError::InvalidLight` y `BlockRegistryBuilder::with_default_blocks`.
- `ChunkData.blocks` es privado (`blocks()`, `blocks_mut()`, `from_paletted()`, `compact()`).
  `ChunkData.light` y `LightStorage` desaparecen. Nuevos: `ChunkData::light_masks`, `may_emit`,
  `has_cached_light_masks`, los tipos `LightMasks`, `MaskRows` y `OwnedLightMasks`, y
  `heap_bytes` cuenta las máscaras construidas.
- Nuevo crate `boxel_light` (depende solo de `boxel_voxel`).

Tests que lo vigilan:

- `boxel_voxel/tests/masks.rs`: máscaras iguales a la fuerza bruta en todas las representaciones
  (Uniform, Packed de 1, 2, 4 y 8 bits, Direct; proptest), los uniformes y los de 1 bit sin
  emisión no guardan nada, la caché se reinicia después de `set` y de `Arc::make_mut`,
  `may_emit` sin falsos negativos, validación del registry.
- `boxel_light/tests/oracle.rs`: mundos aleatorios de 5x5x5 chunks (opacidad 0, 1, 5 y 15,
  emisores RGB opacos y transparentes, techos del contenido, techos muy arriba, pisos sobre aire
  dentro del mundo, debajo del mundo e `i64::MIN`, orígenes cerca y lejos): cada chunk del 3x3x3
  interior da la misma luz que el oráculo global, bit a bit, con y sin caminos rápidos. De punta a
  punta: tiles de la caminata sobre el overworld generado contra el oráculo con los techos de
  fuerza bruta cortados en el piso. Casos a mano: una lámpara en la oscuridad, un pozo con cielo
  15 en el fondo, y una lámpara quitada o fuera de la caja que no impide los caminos rápidos.
- `boxel_light/tests/far_coords.rs`: el mismo vecindario en 0, ±2^31, ±2^40 y
  `MAX_BLOCK_COORD - 64` da los mismos bits.
- `boxel_light/tests/sky_walk.rs`: la caminata igual a un recorrido de fuerza bruta de los chunks
  generados (overworld con árboles y entradas, forest, terrain y flat, cerca y en 2^40), el
  límite, el vacío, la búsqueda después de una edición, una edición 5000 vóxeles arriba que solo
  cambia su columna (la edición aplicada al tile es igual a una caminata nueva), tiles sin piso y
  generadores sin tope.

## Alternativas descartadas

- **B, un volumen por chunk muestreado por píxel**: sin AO (o con el mismo costo en quads que se
  quería evitar), 98 a 317 MiB de volúmenes a radio 32, y Low necesita un parche de wgpu-hal o se
  queda con luz plana.
- **C, bricks dispersos**: el diseño más complejo (tabla interior, 150 baldosas de borde por
  chunk, pool con listas libres), hasta 8 lecturas dependientes por píxel y el mismo problema de
  Low. Su argumento contra hornear suponía 16 bytes por quad y una etapa de luz en serie; A no
  tiene ninguna de las dos.
- **BFS incremental entre chunks al estilo Starlight**: más barato por edición, pero escribe en la
  luz de los vecinos (copias, carreras con mallas en vuelo), necesita BFS de borrado por canal para
  la luz de color, reilumina vecinos cada vez que llega un chunk y depende del orden de carga hasta
  converger.
- **Techo de cielo desde lo cargado** (como Cubic Chunks): depende del orden de carga y produce
  tormentas de reiluminación al moverse en vertical.
- **`sky_floor` por generador (A original)**: no es una cota válida con techo de cuevas 0 ni con
  entradas que dan a cavernas.
- **En High y Mid, un buffer lateral de luz de 8 bytes por quad, o quads de 16 bytes**: el doble
  de memoria de quads (unos 290 MB contra 145 MB a radio 16 bajo tierra). Solo Low lo paga
  (sección 7), porque no puede leer la paleta de un storage buffer.
- **Mezcla aditiva de colores**: necesita un BFS por fuente y satura.
