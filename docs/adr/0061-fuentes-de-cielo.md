# ADR 0061: fuentes de cielo y ambiente por reino

- Estado: aceptado (issue #140, que reemplaza a #130). El dueño aprobó el plan
  (`docs/planes/sky-sources-plan.md`) el 2026-11-02, con la opción A y sus respuestas a las
  preguntas abiertas (sección 9 del plan). Se construye por carriles (sección 8 del plan). Cada
  carril que toca una zona protegida pide su OK cuando le toca. Hechos: el carril 0 (este ADR), el 1
  (los estados de fuente en `boxel_voxel`), el 2 (los rayos en `boxel_light`), el 3 (el mundo, en
  `boxel_world`), el 4 (ambiente y sombreado por reino) y el 5 (depuración, demo y herramientas).
  Falta el 6, el juego.
- Fecha: 2026-11-02

## Contexto

La luz de cielo solo llega desde arriba: un vóxel tiene cielo 15 si está sobre el techo `T(x, z)`
de su columna (ADR 0015, sección 4). Hay lugares donde el juego quiere luz de día y el cielo no
llega:

- una caverna grande bajo tierra que se vea iluminada, con sombras bajo los aleros;
- un rayo de sol que entra por una grieta del techo de una cueva;
- la cara de abajo del reino del techo (ADR 0039, V4), que el plano corta del cielo.

Además hay un solo color de cielo por frame (`view.sky_light`) para todo el mundo. Un reino que
es su propia caverna no puede tener otra curva del día, otro ambiente ni otro sombreado de caras.

#130 proponía un cielo desde abajo (`RealmSky::Under`, una caminata espejada). Solo servía para la
cara de abajo del techo y no daba nada para las cavernas.

## Decisión

### 1. Qué es una fuente

Una fuente de cielo es una celda que emite cielo 15 en un rayo recto por su columna.

- **Dirección**: abajo (por defecto) o arriba.
- **Alcance** `R`, de 1 a `MAX_BEAM` (256 vóxeles, 8 capas de chunks). `R` cuenta la celda de la
  fuente.
- **Rayo**: la fuente y las celdas siguientes en su dirección mientras no tapen el cielo
  (opacidad 0, la misma prueba que da `T`), como mucho `R` celdas, y nunca más allá del plano de
  su reino.
- **Las celdas del rayo son cielo directo**: nivel 15 en el canal del cielo, como un vóxel sobre
  `T`. Siembran la inundación del cielo igual que el cielo directo.
- **Desde el rayo, la luz se esparce como luz de cielo**: pierde `max(1, opacidad)` por celda y la
  frena la opacidad 15. Las paredes y los aleros dan sombra igual que bajo el cielo abierto.
- **Hojas y agua terminan el rayo** (decisión del dueño), como terminan el cielo directo en `T`:
  bajo una hoja hay luz esparcida (14, 13...). Una fuente sobre hojas se ve igual que el cielo
  sobre hojas.
- **Las fuentes no mueven `T`** (tienen opacidad 0) y nunca tocan `SkyTile`. Un rayo sobre `T` no
  suma nada.
- Fuentes y cielo comparten el canal y se mezclan con el máximo. El punto fijo sigue siendo único.

Por qué un rayo y no un emisor: la luz de bloque cae 1 por celda desde el comienzo, así que no pasa
de 15 celdas. Un rayo lleva el 15 por toda la altura de una caverna y se esparce desde cada celda.

### 2. Dónde vive: un estado de bloque

```rust
pub const MAX_BEAM: u16 = 256;
pub enum BeamDir { Down, Up }
pub struct SkySourceDef { pub dir: BeamDir, pub range: u16 }
BlockRegistryBuilder::set_sky_source(state: BlockState, def: SkySourceDef) -> Result<(), RegistryError>
```

- El juego registra un bloque (por ejemplo `game:sky_source`, con estados de dirección y algunos
  alcances) y marca cada estado. El motor nunca lo nombra; los tests usan `test:sky_source`.
- Validación (`RegistryError::InvalidSkySource`): forma `RenderShape::None`, opacidad de luz 0,
  emisión 0, sin colisión y `1 <= range <= MAX_BEAM`. Tampoco un fluido ni un bloque con la
  propiedad `waterlogged`: la fuente comparte su celda solo con aire.
- El juego marca el bloque con `set_keeps_fluids_out` (decisión del dueño): el agua no se lleva una
  fuente que puso la generación.
- `BlockRegistry::sky_source`, `LightTables::sky_source`, `is_sky_source` y `any_sky_source` (una
  clase de luz propia), `ChunkData::sky_sources` (la lista de fuentes del chunk en orden, guardada
  con sus máscaras) y `ChunkData::may_source` (un escaneo de paleta, como `may_emit`).
- La lista no va en la vista `LightMasks`, que cada caja de luz lee 27 veces y que no la
  necesita: solo la leen los tiles de rayos. Con la lista dentro, la vista crecía 16 bytes y
  `masks/cave_1bit` subía un 8 %. Un chunk con una fuente en su paleta nunca se sirve desde sus
  palabras, como uno con un emisor, así que su lista y sus máscaras se construyen juntas una vez.
- `boxel_voxel::testing::add_sky_source` suma `test:sky_source` (dirección abajo o arriba,
  alcances 1, 16, 64 y 256) para los tests de los otros crates.

Así todo lo demás ya funciona: ediciones, `BlockChanged`, lotes atómicos, la versión del chunk, el
guardado por nombre, el streaming y la generación privada. Un chunk con una fuente deja de ser
aire uniforme, así que ningún atajo de "aire uniforme" necesita un caso especial.

Costo aceptado: la fuente ocupa una celda (no puede compartirla con agua ni hojas), y un chunk de
cueva de 1 bit que recibe una fuente pasa a 2 bits y construye sus máscaras una vez.

### 3. Cómo se enteran las cajas: tiles de rayos

Un rayo no es local: una fuente hasta 255 celdas arriba puede iluminar una caja, y un bloqueador en
un chunk que el job de malla no tiene puede cortarlo. Es el problema que resuelve `T`, con la misma
respuesta:

- **`BeamTile`** (`boxel_light`), uno por columna de chunks y exacto para una ventana de y: todas
  las fuentes que alcanzan la ventana, con su rayo entero, por columna (x, z). Se calcula desde el
  contenido (snapshot cargado, chunk guardado, generación privada o aire conocido), nunca desde lo
  que está cargado. Por eso el orden de carga no cambia la luz.
- **La caja** (`LightInput::beams`, por columna de sus 9): `build_direct` suma los rayos a las
  filas de cielo directo. Si algún rayo cruza la caja, `FastDark` no vale. `FastOpen` no cambia. La
  caja sigue siendo exacta: las celdas del rayo son fuentes de nivel 15 como el cielo directo.
- `ChunkLight`, `PaddedLight` y el mesher no cambian.

Lo que construyó el carril 2 (`boxel_light`):

- `beam_tile::build(ventana, planos, tablas, capa)`. Los planos de reino van como un slice simple
  y ascendente, el de `RealmStack::planes`: el plano `p` es la primera celda del reino de arriba.
  Un rayo hacia abajo para en el plano más alto que está en su fuente o debajo, y uno hacia arriba,
  justo antes del primer plano que está arriba de su fuente (`beam_tile::beam_limit`). No se usa
  una closure porque el generador ya da los planos así. El recorte no necesita planos alineados a
  chunks. No hay argumento de columna: la closure `capa` ya sabe qué columna lee, como en
  `sky_tile::walk`.
- El tile guarda cada fuente que alcanza la ventana por su alcance (`beam_tile::reaches`) con su
  rayo entero. Así una edición se retraza en el lugar: `BeamTile::edit(x, z, y, estado, planos,
  tablas, capa)` da `Retrace::Changed { lo, hi }` (las celdas de rayo que cambiaron), `Unchanged` o
  `Unresolved` (le faltó una capa y el tile no cambió). El resultado es igual a un `build` del
  contenido editado. Cortar un rayo no lee ninguna capa; solo crecerlo o poner una fuente.
- El índice por columna es `u32` (4 KiB), no `u16`: una columna puede tener más de 65 535 rayos
  al alcance (fuentes apiladas) y un `u16` se desbordaría. Un tile sin rayos no guarda índice
  (0 B). Cada rayo son 8 B, en offsets de 16 bits desde `w_lo - 256`, así que una ventana tiene a
  lo sumo `MAX_WINDOW` = 65 024 filas. `flags` guarda `range - 1` (bits 0 a 7) y la dirección
  (bit 8); los bits 9 a 15 quedan libres para rayos de color y fuentes constantes.
- `LightInput::beams: [Option<&BeamTile>; 9]`. Cada tile debe cubrir las filas de la caja (se
  revisa en debug). `build_direct` suma los rayos por columna de la caja: los junta en tramos
  disjuntos, marca el comienzo y el final de cada tramo y hace un barrido XOR hacia arriba, sobre
  las filas `closed` que la inundación del cielo pisa después.
- El oráculo traza los rayos celda por celda (`OracleWorld::sources`, `planes`, `beam_cells`).
- Los filtros de luz (ADR 0060): un rayo es cielo directo y queda blanco. Un filtro de opacidad 0
  (vidrio de color) lo deja pasar; uno de opacidad 1 o más lo termina, como a cualquier bloqueador.
- Los otros crates que arman un `LightInput` (`boxel_world`, dos tests de GPU de `boxel_render`)
  solo agregan `beams: [None; 9]`, sin otro cambio. El carril 3 los conecta.

### 4. Costo cero sin fuentes

Una malla pide el tile de una de sus 9 columnas solo si alguna capa a 8 capas o menos puede tener
una fuente: una banda que declara el generador (`WorldGenerator::sky_source_layers`, vacía por
defecto; con WG4 pasa a `ColumnBounds`), un chunk cargado con una fuente en su paleta, o un chunk
guardado sin cargar. Si no, pasa `None`: ningún job y ninguna espera. Un mundo sin fuentes da
mallas idénticas byte a byte a las de hoy.

### 5. Ediciones

En el mismo lote atómico de la edición: poner o quitar una fuente, poner un bloqueador dentro de
un rayo o quitar el que lo terminaba retraza el rayo en el tile y remalla el tramo que cambió, más
o menos 14. Una edición remalla como mucho una banda de `256 + 2 * 15` celdas en 3 x 3 columnas
(unos 90 chunks en el peor caso, casi siempre 3 x 3 x 2). Ese límite es la razón del tope de 256.

### 5.1 Carril 3: cómo quedó

El dueño dio el OK para tocar el streaming y el trait del generador el 2026-11-02.

- **`SourceColumn`** (`boxel_world::beams`) vive dentro de `SkyColumn` como
  `Option<Box<SourceColumn>>`. Se crea cuando una malla pide el tile o cuando un chunk cargado
  puede tener una fuente. En un mundo sin fuentes no existe: cero bytes por columna. Guarda las
  capas cargadas con fuentes, el tile, el tile de antes de un cambio (`old`), la ventana que pide
  el próximo build (`want`), el build en vuelo con su versión y su ventana, los que esperan y el
  lote de la edición.
- **El tile sigue sirviendo mientras se pide uno más ancho.** Si una malla cae fuera de la
  ventana, se pide un build de la unión (`widen`) y la malla espera, pero las mallas que ya
  cubre siguen usando el tile: su contenido no cambió. Si la unión pasa de `MAX_WINDOW` (dos
  viewers a miles de capas en la misma columna), la ventana nueva reemplaza a la vieja y la
  columna queda marcada (`replaced`): desde ahí, una edición que puede tocar filas fuera de la
  ventana remalla todo lo que su rayo alcanza.
- **Un build no genera de más.** Las capas sin cargar de un lado de la ventana donde la compuerta
  dice que no puede haber fuentes se leen como aire: no cambian ninguna celda de rayo de la
  ventana, solo dónde terminan los rayos que salen de ella. Antes de arrancar, el build espera las
  capas cargadas que todavía se generan, como la caminata.
- **Ediciones.** Se retrazan después de aplicar todos los datos del lote, una vez por posición
  con su estado final (la lista de `BlockChanged`). Una fuente nueva lejos de la ventana del tile,
  o en una columna sin tile, pide un build que se compara con "sin rayos": esas mallas se
  hicieron sin tile porque nada podía iluminarlas. La comparación (`beams::changed_band`) mira
  solo la ventana nueva y toma como vacías las filas que la vieja no tenía.
- **Los jobs de luz** (la caché de luz que leen los ticks, ADR 0059) esperan los tiles igual
  que las mallas.
- **Un build que falla** deja un tile vacío (sin rayos) en vez de bloquear las mallas, como una
  caminata que falla deja la columna abierta.
- **Público**: `SkyMap::beam_tile`, `SkyMap::beam_stats` (`BeamStats`: tiles, rayos y memoria),
  los campos de `WorldStats` (`beam_tiles`, `beam_jobs`, `beam_private_generations`,
  `waiting_beams`, `light_beam_boxes`) y una línea `"beams"` del tablero, solo con fuentes en el
  registro. `beam_tiles` se cuenta recorriendo las columnas, también solo con fuentes.
- **Generadores**: `DimensionGenerator::sky_source_layers` es la envolvente de las bandas de sus
  reinos, cada una dentro de su reino. `RegionGenerator` deja la banda vacía: un juego que pone
  fuentes desde una etapa de región tiene que envolverlo y declarar su banda (carril 6).
- **Medir**: `headless_world` imprime una línea `beams:` y tiene `--sky-sources`, un arnés de
  medición (no una demo): una fuente hacia abajo de alcance 64 en cada otra columna bajo cada
  techo de cueva, a 24 capas o menos del viewer.

### 6. Ambiente y sombreado de cara por reino

Una fuente toma la curva del reino donde está. Una caverna que es su propio reino tiene su propia
curva (por ejemplo constante, sin noche). Una zona dentro del reino de la superficie se oscurece
de noche con el cielo.

- **`RealmLighting`** (`boxel_world`): por reino de la dimensión, una `DaylightCurve` (`None`: la
  global), un color de ambiente RGB y 6 factores de sombreado de cara (+Y, -Y, +X, -X, +Z, -Z). Los
  valores por defecto son los de hoy: ambiente 0.02 en los tres canales y 1.0, 0.5, 0.8, 0.8,
  0.65, 0.65. El juego la llena desde su tabla de entornos (ADR 0039, sección 16).
- **Qué reino ilumina un fragmento**: el del vóxel delante de su cara, el que da su luz. Una cara
  de arriba en la última capa bajo un plano toma la curva del reino de arriba.
- **Uniform**: `realm_planes` (8 planos relativos a `render_origin.y`) y `realm_env` (9 entradas
  de 3 `vec4`: cielo por luz del día, ambiente y sombreado). `View` pasa de 352 + 8192 a 816 + 8192
  bytes, todavía un solo uniform block en Low.
- **Shader**: `finish_vertex` busca el reino (8 comparaciones enteras) y lo guarda en los bits
  libres de `VsOut::ao`. `voxel_light` lee `realm_env[reino]` en vez de `sky_light`, y `face_shade`
  lee su factor de la misma entrada. Igual en `cell.wgsl`, `lod_common.wgsl` (el reino dibujado) y
  los modelos (el poser de CPU toma el sombreado del reino de la instancia).
- **Píxeles idénticos**: en un mundo de un reino todo fragmento toma el reino 0, con los mismos
  `f32` de hoy y los productos en el mismo orden. Toda imagen de hoy sale igual bit a bit en los
  tres tiers.
- La niebla, el color de fondo y el LOD siguen por cámara, en el juego.

### 6.1 Carril 4: cómo quedó

- **`View`**: 816 + 8192 bytes (9 008), un solo uniform block en Low. Suma `realm_planes` (2
  `vec4<i32>`: los 8 planos relativos a `render_origin.y`, recortados al rango de `i32`; los que no
  se usan valen `i32::MAX`) y `realm_env` (9 entradas `RealmEnv` de 3 `vec4`: cielo y sombreado de
  +Y, ambiente y sombreado de -Y, y los sombreados de +X, -X, +Z y -Z). El reino que dibuja el LOD
  va en `light_params.w`, que estaba libre.
- **El vóxel de delante**: `finish_vertex` toma la `y` del vóxel delante de la cara (la del quad
  más la normal, en enteros) y guarda el reino en los bits 12 a 15 de `VsOut::ao`. Los muros de
  borde, las cruces y los planos toman su propia celda, como el mesher. Excepción conocida: una
  cara ±Y de un modelo de cajas dentro de su celda, con un cubo opaco delante, toma la luz de su
  propia celda en el mesher, pero el shader no lo sabe y usa la de delante. Solo cambia algo en la
  capa junto a un plano.
- **Celdas** (`cell.wgsl`): un varying plano nuevo (location 3) con el reino del chunk. Los planos
  están alineados a chunks, así que el reino del chunk es el de la celda.
- **Modelos**: el reino va en los bits 24 a 27 de `ModelVertex.data` y sale del bloque donde se
  mide su luz. Su sombreado es el de entidades de Minecraft, no la tabla de caras, así que el poser
  usa `realm_shade`: escala la oscuridad de cada cara en la misma proporción en que el reino
  escala la de las caras del cubo, mezclada por los ejes de la normal. Con la tabla por defecto da
  lo de hoy bit a bit, y con una tabla plana da un modelo plano. Una cara que mira justo arriba
  queda en 1.
- **Recursos**: `RealmPlanes` es por mundo (se publica al abrir y se limpia al cerrar).
  `RealmLighting` es de la app, como `DaylightCurve`, y está en `DaylightPlugin`. Sin reloj, las
  curvas de los reinos se toman al mediodía.
- **Sigue global**: el color del cielo, la niebla y el color de fondo son los de la curva global.
  Cambiarlos por reino es cosa del juego, por cámara.
- **Píxeles idénticos**: de las 2237 imágenes de las pruebas de GPU, 2234 salen iguales byte a
  byte que en main. Las otras 3 son del tier High (llvmpipe y la RTX) y cambian un píxel de borde
  entre pasto y tierra en 1 o 2 niveles, siempre igual de una corrida a otra. Es el solape de
  orden de dibujo de High que ya acepta el ADR 0015, sección 12.

### 7. Por qué no cubrir el cielo con fuentes

El cielo abierto sigue implícito: un techo por columna. Un rayo empieza en su fuente (el aire de
arriba no se ilumina), llega a 256 celdas como mucho (habría que saber dónde está el suelo, que es
`T`) y sin tope una edición remallaría kilómetros. Una fuente cuesta además el doble de memoria que
un techo y convierte un chunk de aire gratis en uno real. Las alturas extremas las arregla #129
(EW-REALMSKY) con comienzo y piso por columna.

### 8. Opción B, después

Una zona con su propia curva dentro de un reino (una caverna de la superficie que no se oscurece
de noche) no la pide la decisión. Si algún día hace falta, va como "fuentes constantes", que
escriben su rayo en los canales de bloque (S a M sobre esto), no como un tercer canal de luz (XL).

### 9. Carril 5: cómo quedó

- **F3** (`boxel_debug`): una línea `Realm 0 of 1  curve global  sky 1.00 1.00 1.00` con el reino
  del ojo, si su curva es propia o la global y su luz de cielo ahora. Con fuentes en el registro,
  una línea `Source 3 74 -2  down range 64  beam 11..74` con la fuente cuyo rayo pasa por la celda
  del `CrosshairTarget` (la de delante de la cara apuntada, la que da su luz). Sale `Source none`
  si no pasa ningún rayo y `Source -` si ningún tile cubre esa celda todavía. Las fuentes no se
  apuntan (no tienen contorno); el modo para elegirlas es del juego.
- **`SkySourceMarkers`** (`boxel_debug`, apagado por defecto): un cuadrado sobre cada fuente y un
  punto cada 2 celdas de su rayo, a 48 bloques del ojo o menos, debajo del texto y con el texto
  oculto también. Lee los tiles que construyó el mundo (`SkyMap::beam_tile`) y proyecta con
  `CameraView` en enteros relativos al origen de render, así que sirve a cualquier coordenada.
  Son rectángulos de `OverlayDraw`: no toca el render. Como mucho 16 384 marcas por frame.
- **La fuente de la demo** (`boxel_demo::blocks`): `boxel:sky_source` con `dir` (`down`, `up`) y
  `range` (16, 64, 256; por defecto abajo y 64), invisible, sin colisión y con
  `set_keeps_fluids_out`. No está en `registry()`: un registro con fuentes hace que el mundo
  cuente sus tiles, así que la agregan solo las demos de fuentes (`registry_with_sky_source`),
  después de los otros bloques, y ningún otro id cambia.
- **`LitCaves`** (`boxel_demo::worldgen`): envuelve cualquier generador y pone una fuente en la
  primera celda de aire bajo cada techo de cueva, en cada otra columna, en una banda de capas que
  declara en `sky_source_layers`. Reemplaza el arnés `CaveSources` de `headless_world`, que ahora
  usa esta versión con `--sky-sources`.
- **`voxel_world --sky-sources`**: una caverna cerrada de 21 x 21 x 12 sobre el suelo del comienzo,
  con una fuente en cada columna bajo el techo, un pilar, un alero contra la pared oeste y una
  puerta al sur. La cámara empieza adentro (salvo con `--camera` o `--walk`). B prende y apaga los
  marcadores. La caverna está en el reino de la superficie, así que se oscurece de noche.
- **`stress --sky-sources`** usa `LitCaves` a 24 capas del comienzo. Con `--edits`, el guion suma
  cuatro pasos: pone una fuente sobre el suelo, tapa su rayo a mitad de camino, lo destapa y quita
  la fuente.
- **`lit-cavern`** (`tools/perf`): `cave-static` con `--sky-sources`.

## Medidas

- Carril 1, criterion `masks`, una corrida por lado contra a9a0abf, una detrás de la otra:
  `cave_1bit` 4,52 a 4,50 ns (sin cambio), `surface_packed` 12,09 a 12,21 µs (+0,9 %, dentro del
  ruido). Nuevo: `cave_with_sources` (la cueva de 1 bit con 434 fuentes bajo sus techos, 2 bits)
  16,8 µs para construir máscaras y lista.
- Carril 4, `just perf-smoke surface cave-static` (RTX, radio 16, sin tope) contra a9a0abf:
  superficie 2020 a 2058 fps (+1,9 %), cueva quieta 2474 a 2463 (-0,4 %), p99 igual. Ok.
- Carril 2, criterion `light`, una corrida por lado contra 641774b, una detrás de la otra. Sin
  rayos (`beams: [None; 9]`) nada empeora: `box_cave_dark` 41,7 a 41,5 ns (sin cambio),
  `walk_generate` 130,9 a 129,7 µs y `walk_loaded` 16,5 a 16,2 µs. Las cajas con inundación
  salieron entre 8 y 15 % más rápidas (`box_surface` 47,6 a 40,3 µs), que es ruido de la máquina:
  el cambio no toca la inundación.
- Carril 3, `headless_world --world overworld --view-distance 16 --viewer-y -1000`: sin
  fuentes, 17 727 chunks, 12 981 mallas y 9 050 803 quads, cero tiles de rayos, cero jobs y cero
  esperas (las mallas son las de main bit a bit: lo fija un test). Con `--sky-sources` (una
  fuente en cada otra columna bajo cada techo de cueva, a 24 capas del viewer) los mismos chunks
  y 16 829 291 quads (1,86 veces), 933 tiles con 2,5 millones de rayos (23,3 MiB), 2 007 builds y
  31 783 generaciones privadas: las fuentes llegan hasta 8 capas fuera de lo cargado. Toda caja
  toma el camino completo (13 546) y la caché de luz pasa de 0 a 31,4 MiB.
- Carril 3, `just perf-smoke cave-static cave-path` (RTX, radio 16, sin tope) contra 4eeb410:
  cueva quieta 2488 a 2484 fps (-0,2 %), ruta en cueva 2542 a 2532 (-0,4 %), p99 igual. Ok.
- Carril 5, `just perf-smoke cave-static lit-cavern` (RTX, radio 16, sin tope) contra e5a796e:
  cueva quieta 2338 a 2450 fps (+4,8 %), p99 0,93 a 0,87 ms. Ok. `lit-cavern` es nuevo y la
  base no lo tiene, así que el smoke lo salta. Para tener su número se midió el mismo binario con
  y sin la opción (`perf.py smoke --base HEAD --new-extra=--sky-sources cave-static`): 2432 a
  1647 fps (-32 %), p99 0,88 a 1,71 ms, ningún frame de más de 6,94 ms, y 8 926 346 a 16 607 799
  quads (1,86 veces). El smoke lo marca peor, como debe: es el costo de iluminar todas las cuevas
  a 24 capas, no del código. Es la base de las próximas corridas de `lit-cavern`.
- Nuevos, sin lado base:

| Bench | Tiempo | Qué mide |
|---|---|---|
| `light/box_cavern_ceiling` | 36,2 µs | caverna de 64 de alto, una fuente bajo el techo en cada columna, dos pilares (9 198 rayos en los 9 tiles). Meta: 100 µs o menos |
| `light/box_cavern_ledge` | 167,6 µs | la misma con un alero de lado a lado: 26 390 celdas en su sombra. Es el costo de inundar esa sombra, como cualquier caja con mucho cielo esparcido |
| `light/box_sunbeam` | 25,2 µs | una cueva generada con un rayo de sol |
| `beam_tile/build_loaded` | 31,1 µs | la columna de esa cueva, 19 capas cargadas con sus máscaras, 5 132 fuentes bajo los techos de cueva, 1 412 rayos guardados (15 KiB) |
| `beam_tile/build_loaded_cold_masks` | 375,7 µs | lo mismo sin máscaras: cada capa con una fuente las construye (unos 17 µs, carril 1). Las cajas de luz las reusan después |
| `beam_tile/build_generate` | 3,16 ms | cada capa generada y decorada en la closure: 166 µs por capa, casi todo generación |

## El juego

- La generación pone las fuentes: en el primer aire bajo el techo de una caverna, en cada columna o
  cada dos, hacia abajo; un rayo de sol bajo una grieta; en la cara de abajo del reino del techo,
  alcance 32 a 64. El generador declara dónde las pone (`sky_source_layers`).
- Un ítem creativo por estado de fuente, que pone y quita fuentes y muestra sus marcadores.
- F3 muestra la fuente apuntada y el reino con su curva, y `SkySourceMarkers` (apagado por
  defecto) dibuja fuentes y rayos cerca de la cámara.

## Consecuencias

- Una caverna iluminada entera cuesta quads como un bosque (hasta 1,74 veces bajo tierra) y
  paletas como la superficie: las caras iluminadas pierden la elisión de AO de las caras oscuras.
  Una cuadrícula rala de fuentes lo baja. El carril 3 lo mide.
- Una caja con un rayo cerca toma el camino completo (unos 56 µs, como la superficie) en vez de
  `FastDark` (0,04 µs). Las cajas sin rayos no cambian.
- Un tile de rayos cuesta 4 KiB más 8 B por fuente (0 B si no tiene rayos). Un techo lleno de
  fuentes en toda la vista son unos 13 MiB a r16.
- Sin formato de guardado nuevo: las fuentes son bloques guardados por nombre, los tiles no se
  guardan y `RealmLighting` es del juego.
- `RealmSky::Under` desaparece: #129 quita la variante y el reino del techo pasa a `Closed` con
  fuentes (ADR 0039, sección 15).
- Con `NullChunkStore`, una fuente creativa en un chunk que se descarga se pierde con sus
  ediciones, como un techo de cielo editado hoy.

## Alternativas descartadas

- **Datos por celda (`BlockData`).** No pueden vivir en el aire y un cambio no remalla (ADR 0048).
  Igual haría falta un bloque que los lleve.
- **Una lista de puntos por chunk.** No ocupa celda, pero pide un tipo de edición nuevo, un
  mensaje nuevo, un campo en el registro del chunk y un chequeo en cada atajo de aire uniforme,
  todo en zonas protegidas.
- **Un emisor de bloque fuerte.** No pasa de 15 celdas y no sigue el día.
- **Cielo desde abajo (#130, `Under`).** Solo ilumina la cara de abajo del techo, con una caminata
  espejada que nadie más usa. Las fuentes cubren ese caso y las cavernas.
- **Un tercer canal de luz** para curvas por zona. Duplica la memoria de luz y el formato de la
  paleta en la GPU (XL).
