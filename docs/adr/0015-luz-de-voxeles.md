# ADR 0015: luz de vóxeles

- Estado: aceptado (fases P0 a P6 hechas, y los arreglos de aspecto de la sección 12; P7,
  medir con la máquina libre, y P8, pendientes)
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
- Contenido de fábrica: bloques opacos 15, hojas y agua 1, aire 0. En P0 no había emisores; P6
  suma las lámparas de color y la lava provisoria (sección 10).
  `BlockRegistryBuilder::with_default_blocks` permite agregar bloques después de los de fábrica.
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

### 4. Techo de cielo por columna (P1 y P3, hecho)

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

Debajo del rango del generador solo cuenta el contenido: un chunk cargado sin bloqueadores se lee
igual que uno sin cargar (aire, o vacío si no hay contenido más abajo). Si no, el techo dependería
de qué chunks de aire están cargados.

Ediciones (P3): poner un bloqueador encima de `T` sube `T` (`SkyTile::raise`). Quitar el bloque de
`T` busca el siguiente hacia abajo en los datos cargados (`SkyTile::rescan_column`); si la
búsqueda sale de lo cargado o baja del rango del generador, la columna vuelve a `Pending` y corre
una caminata. El caso que quedaba abierto en P1, un mundo cuyas columnas llegan al vacío: ahí el
contenido debajo del piso decide si las columnas abiertas del tile terminan en el piso o siguen
abiertas para siempre, así que toda edición de un bloqueador debajo del piso manda la columna a
caminar de nuevo. Así los techos de un tile editado son siempre los de una caminata nueva.
`LayerData::AirUntil` salta de una vez un tramo de aire (una plataforma a 2^50 vóxeles de altura
no recorre 2^45 capas).

### 5. Caja exacta por chunk (P1, hecho; corre dentro del job de malla desde P3)

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

### 6. Malla: `PackedQuad` v2 con paleta de luz (P2, hecho)

- El quad sigue en 8 bytes. `geo` usa sus 4 bits reservados como flags (bits 28 a 31; el bit 0,
  `FLAG_EMISSIVE`, marca los emisores desde la sección 12). `mat`
  pasa a `capa:12 | ao:8 | índice de paleta de luz:12`. Desaparecen los campos cielo, bloque y
  flags de `mat` (`MAT_SKY_SHIFT`, `MAT_BLOCK_SHIFT`, `MAT_FLAGS_SHIFT`); entran
  `GEO_FLAGS_SHIFT = 28`, `MAT_LIGHT_SHIFT = 20` y `MAT_LIGHT_MASK = 0xFFF`.
- Paleta por chunk de hasta 4096 entradas de 8 bytes (`[u32; 2]`: las 4 esquinas en
  `LightValue`, en el orden de esquinas del AO y de `common.wgsl`), en orden determinista de
  primera aparición. Si se pasa de 4096, el chunk se vuelve a mallar con los canales de bloque a
  3 bits (`LightQuantization::Block3`). Eso tampoco garantiza que entre (quedan 8192 valores por
  esquina), así que hay un último nivel, `Flat`: cada cara toma una sola luz, el promedio de sus
  esquinas con el bloque a 2 bits, que siempre entra (1024 valores). `ChunkMesh` guarda el nivel
  y `headless_world` cuenta las mallas recuantizadas.
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
- Implementación: `MeshInput.light` (`Option<&PaddedLight>`, `None` es cielo 15 en todo),
  `MeshOptions { ao: AoPolicy }`, `Mesher::mesh(padded, light, materials, options, out)`. Con luz
  uniforme la clave es el material (y el AO si va), sin tablas. Con luz completa, cada cara
  visible lee sus 9 celdas de enfrente y busca su clave (material, AO, 4 esquinas) en una tabla
  de direccionamiento abierto; la paleta solo se consulta para claves nuevas.
  `reference_face_shading` es el oráculo de fuerza bruta de AO y luz por cara.
- La política es `WorldSettings::ao_policy`. Como el AO lee vecinos de arista y esquina, una
  edición remalla todo chunk cuya región acolchada contiene el vóxel (hasta 8), no solo los
  vecinos de cara.

### 7. Mundo (P3, hecho)

`boxel_world` calcula la luz dentro del job de malla. Sin estado nuevo por chunk, salvo la caché
de gameplay.

- **`SkyMap`** (recurso, `src/sky.rs`): un `SkyColumn` por columna de chunks con algún slot
  cargado, creado con su primer slot y borrado con el último. Estado `Pending`, `InFlight
  { version }` o `Exact(Arc<SkyTile>)`, más `version`, `edited` y el bloqueador editado más alto
  (el comienzo `S` de la próxima caminata). Un resultado cuya `version` cambió se descarta y la
  columna se camina de nuevo. Guarda también las capas de sus slots cargados, ordenadas: la
  caminata, las bandas y la búsqueda del techo siguiente recorren solo esas capas, nunca el rango
  numérico (un viewer a y = -2^40 en un mundo que llega al vacío, o una edición muy arriba de
  otro viewer, no recorren 2^35 capas).
- **Caminata perezosa** (`JobKind::SkyTile`, `JobId::Column`): una columna se camina solo cuando
  una malla la necesita, así que un viewer hondo, con todas sus cajas debajo del piso, no camina
  nada. Comparte el tope de jobs en vuelo y su prioridad es la de su slot más cercano al viewer.
  Antes de arrancar espera los datos de las capas cargadas que va a leer (desde `S` hasta abajo
  de la malla más baja que la espera), así casi no genera nada por su cuenta: lo que sí genera lo
  cuenta `WorldStats::sky_private_generations`. Las capas cargadas las lee del snapshot (ahí
  están las ediciones).
- **Regla del mundo** (fija al arrancar): con `light` apagada no hay luz (mallas con cielo 15).
  La imagen de antes necesita además `ao_policy: AoPolicy::Off`: sin luz toda cara cuenta como
  iluminada y con la política por defecto lleva AO. Con un generador sin tope (`GeneratorTop::Unbounded`), luz sin techos: toda
  columna abierta, con un aviso en el log. Con un generador vacío, solo las ediciones tapan el
  cielo (sin piso). Con tope, el piso es el tope menos `sky_walk_limit`.
- **`start_mesh`**: si la caja del chunk (su origen más 47) está en el piso o arriba, o si el
  mundo puede llegar al vacío, espera que las 9 columnas de alrededor estén `Exact`; la malla
  queda `Queued` y se agenda cuando llega el tile (`WorldStats::waiting_sky`). Debajo del piso
  usa `SkyInput::BelowFloor`, sin tiles. El job toma un `LightScratch` y un `PaddedLight` por hilo,
  corre `boxel_light::compute` y después malla. Dentro de `light_cache_radius` (8 chunks) guarda
  también el `ChunkLight` del chunk, que lee `ChunkMap::light_at`.
- **Caché de luz**: la luz guardada lleva la versión de datos y el `epoch` con que se calculó, y
  solo es actual si siguen iguales (un job de malla fallido ya no deja luz vieja como actual). Un
  chunk con malla dentro del radio cuyo job de malla no la guardó (todo aire, encerrado, un job
  fallido, o que entró al radio después de mallarse) recibe un job de luz (`JobKind::Light`,
  `WorldStats::light_jobs`): calcula solo la luz, con prioridad detrás de las mallas de la vista.
  Un chunk uniforme de un bloque opaco sin emisión queda oscuro sin job. Cuando un viewer cambia
  de chunk se recorren las cáscaras que entran y salen del radio: las que entran piden su luz,
  las que salen la sueltan. `light_cache_radius` es como mucho 32.
- **Publicación**: una malla igual a la publicada (mismo hash y mismos bytes) no se envía de
  nuevo. Las mallas de un mismo lote de ediciones salen en el mismo frame: las que llegan en el
  frame N, de tiempo t, esperan al resto hasta el primer frame que es a la vez de t +
  `edit_batch_hold` (16 ms) o después y N + 2 o después; si el resto tarda más, salen en ese
  frame como están (`WorldStats::edit_batches_split`). Hasta el 2026-10-07 la espera era solo de
  2 frames (sección 12). Un lote al que no le llegó ninguna malla se cierra pasados 120 frames y
  1 s (las dos cosas), y también cuenta como partido. Los resultados de un lote se
  aplican antes que los del streaming, fuera del tope de eventos por frame, y sus jobs (y la
  caminata que una edición manda a repetir) mantienen prioridad 0 aunque el viewer se mueva. Dos
  lotes que tocan el mismo chunk se juntan; si el chunk tenía su malla retenida, esa malla ya es
  vieja: se descarta y el chunk se remalla enseguida.
- **Ediciones** (`apply_edits`), por vóxel cambiado `p`, todo en un lote con prioridad 0 y
  `epoch` subido:
  - Caras y AO: todo chunk cuya región acolchada contiene `p` (hasta 8).
  - Si cambió la opacidad o la emisión: todo chunk cuya región acolchada toca la caja `p ± r`,
    con `r` el alcance del cambio: el nivel más alto por canal de la luz vieja en `p` y sus 6
    vecinos, y de la emisión vieja y la nueva, como mucho 14. La luz vieja sale de las cachés,
    leídas antes de aplicar el lote; si falta una o no es la del contenido actual, `r` es 14.
    Excavar en una cueva oscura remalla solo el chunk (o los de su región acolchada).
  - Si se movió el techo: todo chunk cargado cuya región acolchada toca la banda de la columna,
    14 vóxeles alrededor, desde 14 debajo del techo más bajo hasta 14 encima del más alto. Si la
    columna vuelve a caminar, el tile nuevo se compara con el viejo y remalla las bandas de las
    columnas que cambiaron, en el mismo lote.
  - Descarga: sin persistencia, un chunk cuya edición movió un techo se lleva la edición, así
    que su columna vuelve a caminar y remalla lo que cambie. Con `PersistPlugin` (ADR 0024) la
    edición queda guardada y la columna conserva sus techos; las caminatas leen los chunks
    guardados, así que no hace falta guardar techos.
  - Mensajes: la lista de cambios por vóxel que mide estos alcances (estado antes de la
    primera edición de cada posición y después de la última) es también la que escribe los
    `BlockChanged { pos, old, new }` del mundo, uno por posición que cambió, en el orden de su
    primera edición. Los lee la física (despierta los cuerpos dormidos cerca, ADR 0016); la luz
    no los lee, se recalcula con el lote. En el frame en que se escribe (después de
    `WorldSystems::Edits`), el mapa ya tiene el bloque nuevo, pero `ChunkMap::light_at` y las
    mallas pueden ser los de antes hasta que llegan las remallas del lote. Quien lo lee en un
    schedule fijo (la física) lo recibe igual, pero quizá frames después: en el medio otra
    edición puede cambiar el bloque o descargarse el chunk, así que lee el mapa en vez de
    confiar en `new`.
- **Coordenadas**: las cuentas de bandas y cajas usan `i64` con resta saturada; los techos se
  comparan relativos a la caja dentro de `boxel_light`. El mismo mundo en x = 2^40 e y = -2^40
  da los mismos bits.
- **`reference`**: `Reference` recalcula tiles, luz y mallas desde cero (datos cargados y
  generador, sin cachés ni caminos rápidos). Los tests comparan contra ella.

### 8. GPU (P4, hecho)

- La paleta vive en el arena de quads, justo después de los 8 buckets del chunk:
  `light_base = quad_base + total_quads`, en un relleno libre de `ChunkGpu` (offset 52; sigue en
  64 bytes), y `light_len`, las entradas de la paleta (offset 56). High y Mid la leen del mismo
  storage buffer: ningún binding nuevo. Un índice que no es menor que `light_len` lee cielo
  abierto, como en Low y en la CPU, nunca los datos de otro chunk. Cada chunk toma
  `boxel_render::arena_len` entradas de 8 bytes: sus quads más su paleta, con al menos una entrada
  (una malla hecha a mano sin paleta recibe una de cielo abierto, así ningún índice lee otro
  chunk). `RenderStats::arena_used_quads` cuenta esas entradas. La subida escribe los quads y la
  paleta en la misma copia.
- Low recibe la paleta expandida por quad en un segundo vertex buffer por instancia
  (`Uint32x2` en `@location(1)`), paralelo al arena con los mismos offsets: un atributo, no un
  binding, así que la regla de ADR 0011 se mantiene. `boxel_gfx::tier::floors::VERTEX_BUFFERS`
  pasa de 1 a 2 (en todos los tiers, como el resto de los pisos). Low reserva ese buffer entero
  al crear el renderer, del tamaño del arena (64 MiB con los 8 Mi de entradas por defecto, haya
  quads o no): un costo fijo de VRAM. Cada tramo de mallas que sube tiene una segunda copia a ese buffer; la entrada de
  cada quad se arma al escribir el buffer temporal (un índice fuera de la paleta lee cielo
  abierto).
- `ViewUniform` pasa de 240 a 336 bytes (352 desde la sección 12; solo `vec4`, std140):
  `sky_light` (color del cielo por
  luz del día, y piso de ambiente), `light_params` (fuerza del AO, 0.55 por defecto, igual al
  `mix(0.45, 1, ao)` de antes; ganancia de luz de bloque) y `light_curve` (16 niveles).
- Fragment (`common.wgsl`: `corner_light`, `level_curve`, `voxel_light`, `ao_factor`,
  `shade_fragment`): interpolación bilineal por canal de las 4 esquinas en (s, t) del quad,
  escrita como `a + (b - a) * s` para que 4 esquinas iguales den su valor exacto;
  `luz = max(curva(cielo) * sky_light.rgb, curva(rgb) * ganancia, ambiente)`, con la curva
  indexada en el uniform (una copia local indexada en tiempo de ejecución puede ir a memoria
  scratch; su costo se mide en P7);
  `color = albedo * sombreado de cara * mix(1 - fuerza_ao, 1, ao / 3) * luz`, con el AO también
  bilineal (antes era lineal por triángulo), después niebla. Con cielo 15 al mediodía y AO
  abierto da la imagen de antes. Igual en todos los tiers. `VsOut` cambia `shade` por la cara,
  (s, t), la entrada de paleta y el byte de AO (los tres enteros, planos).
- Rust: `SceneLight` (color del cielo por la luz del día, ambiente, fuerza del AO, ganancia y
  curva) va en `ViewParams::light`; `ViewParams::clear_color` es el color de fondo del frame (el
  renderer ya no lo toma de su configuración: `RendererConfig::clear_color` desaparece y
  `DEFAULT_SKY_COLOR` es el de antes). `light_curve(gamma)` es la curva de Minecraft
  `f / (4 - 3f)`, llevada hacia `1 - (1 - b)^4` por la gamma; por defecto gamma 0 (la curva
  "moody"). `RenderSettings` suma `ao_strength`, `block_light_gain` y `light_gamma`; su
  `clear_color` queda como el cielo de las apps sin reloj.

### 9. Día y noche (P5, hecho)

Solo un uniform: sin remallado y sin subidas.

- `boxel_world::TimeOfDay { tick, ticks_per_day, paused, speed }` avanza `speed` ticks por paso
  de `FixedUpdate` (64 Hz; un día de 20 minutos son 76 800 ticks), así el reloj no depende de los
  fps. El tick 0 de cada día es la salida del sol (06:00), un cuarto de día el mediodía y tres
  cuartos la medianoche. Empieza al mediodía. `scrub` mueve la hora dentro del día.
- `DaylightCurve` da, según la altura del sol (el seno de la fracción del día), la luz del cielo
  (tinte por intensidad), el color del cielo (fondo y niebla) y el piso de ambiente: día blanco a
  1.0 desde que el seno llega a 0.25, atardecer cálido (1.0, 0.75, 0.55) a 0.6 con el sol en el
  horizonte, noche azul a 0.2 desde -0.2, con smoothstep entre ellos; el amanecer es igual al
  atardecer. El tinte de noche es el (0.30, 0.35, 0.55) del plan escalado para que su canal más
  brillante valga 1, así "20 % de brillo" es el brillo real del azul. El ambiente es 0.02 a toda
  hora.
- `DaylightPlugin` (en `DefaultPlugins`) agrega los dos recursos y el sistema. El `Extract` del
  render arma la `SceneLight` y el color del cielo del frame con ellos; sin el plugin queda un
  mediodía con `RenderSettings::clear_color`.
- `voxel_world`: `[` y `]` mueven el reloj una hora, T lo pausa, F lo acelera 60 veces (G es
  caminar) y `--time HH:MM` elige la hora de inicio; las corridas con `--frames` dejan el reloj
  quieto.
  `stress --time HH:MM` fija la hora y el reloj queda quieto toda la corrida.

La luz de bloque se ve igual al mediodía y a medianoche (lo verifica `gpu_terrain`).

### 10. Contenido y herramientas (P6, hecho)

- Emisores de cubo completo en `BlockRegistry::default_blocks` (estados 12 a 16, después de los
  de antes): lámparas naranja (14, 11, 6), roja (14, 2, 2), azul (3, 6, 14) y blanca
  (15, 15, 15), y la lava provisoria (15, 7, 2), opaca y sólida hasta que haya fluidos
  (`boxel_voxel::LAMPS`, `LAVA_EMISSION`). Cada uno tiene su textura con puntos brillantes, de
  su color. Hasta el 2026-10-07 su cara se iluminaba con la luz del vóxel de enfrente (la
  emisión menos 1) y no parecía encendida; ahora brilla (sección 12). Las antorchas esperan a
  las formas que no son cubos.
- Overlay F3: la luz (cielo y RGB) del vóxel del ojo y del vóxel al que apunta la mira
  (`boxel_debug::CrosshairTarget`, que las apps con picking llenan con la celda delante de la cara
  apuntada), y la hora del reloj. La luz sale de `ChunkMap::light_at` (dentro de
  `light_cache_radius`, también en chunks de puro aire; afuera, o mientras su job de luz no
  termina, muestra `-`).
- `voxel_world`: las lámparas en las teclas 7 a 0 y `--lamps`, que pone lámparas y lava alrededor
  del inicio y empieza a las 21:00.
- `stress --edits`: una edición por segundo durante la medición (poner una lámpara y sacarla,
  cavar un pozo de 24 bloques y taparlo); el resumen da los frames hasta que el mundo vuelve a
  estar quieto y los lotes retenidos o partidos. `headless_world` suma `arena_entries` a su línea
  `light:` (quads más paletas).

### 11. Más adelante (P8)

AO desde ocupación en GPU en High y Mid si el gate G1 o las mediciones lo piden; cascadas de
sombras del sol en High (profundidad con solo `geo`, el cielo horneado como máscara de
exterior); luces dinámicas en el shader; vidrio teñido.

### 12. Arreglos de aspecto (2026-10-07)

Tres cosas se veían mal después de P7. Las tres se arreglan sin quads nuevos: las mallas tienen
los mismos quads que antes.

- **Los emisores brillan.** La cara de una lámpara tomaba la luz de la celda de enfrente por el
  sombreado de cara, como cualquier bloque: no parecía encendida. Ahora el mesher marca los quads
  de los bloques con emisión con `FLAG_EMISSIVE` (bit 0 de los flags de `geo`;
  `MaterialTable::emissive`, `PackedQuad::is_emissive`), y `shade_fragment` dibuja esos quads con
  su textura a brillo pleno: sin luz, sin sombreado de cara y sin AO, como Minecraft dibuja los
  bloques que brillan. La niebla sigue. La textura de cada lámpara ya es de su color, así que se
  ve del color de su emisión, de día y de noche. Los demás bloques hacen la misma cuenta que
  antes, en el mismo orden. El flag no entra en la clave del merge: sale del material, que ya
  está. Viaja al fragment en el mismo entero que el AO (`VsOut::ao`, bits 8 a 12), sin otro
  varying. ADR 0036 lo cambia: brillan a pleno solo los estados marcados.
- **Lotes de ediciones enteros sin tope de fps.** La espera de la sección 7 era de 2 frames. Sin
  tope un frame dura menos de medio ms, menos que un job de malla con luz, y en P7 salían
  partidos 1 a 3 lotes de 20. Ahora es un tiempo con un piso de frames:
  `WorldSettings::edit_batch_hold`, un `Duration` de 16 ms por defecto
  (`WorldSettings::DEFAULT_EDIT_BATCH_HOLD`), reemplaza a `edit_batch_hold_frames`, y la espera
  dura además al menos 2 frames (`MIN_HOLD_FRAMES`), como antes. El piso es para 60 Hz o menos:
  ahí 16 ms son un solo frame, y a un miembro atrasado le pueden faltar dos pasos de jobs (su job
  esperaba lugar en el scheduler y recién arranca en el siguiente, o su remallado esperaba la
  caminata de cielo). Un lote que se completa antes sale apenas llega su última malla, como
  antes, así que con tope 144 no cambia nada: los lotes se completan en 1 o 2 frames, y uno que
  se atrasa espera como mucho 3 frames (20.8 ms) en vez de 2. El tiempo de un frame se toma al
  empezar su paso de jobs (`WorldSystems::Dispatch`, `WorldJobs::begin_frame`); los tests lo
  manejan con un reloj manual, y el harness de `streaming/tests.rs` avanza 8 ms por frame. El
  cierre de un lote al que nunca le llegó una malla también pasó a tiempo: 120 frames y 1 s,
  las dos cosas (sin tope, 120 frames eran 40 ms, menos que una caminata de cielo con la cola
  llena), y ahora cuenta en `edit_batches_split`, porque sus mallas salen sueltas. Si el único
  resultado retenido de un lote se va (su chunk se descarga), se lleva el reloj de la espera;
  antes el lote lo conservaba, y al juntarse con otro lo hacía salir partido antes de tiempo.
- **Sin puntitos de cielo.** Los quads del greedy se tocan en uniones en T: una esquina de un
  quad cae sobre el borde de otro, en el mismo plano o en la arista de un bloque. El rasterizador
  redondea cada vértice a su grilla de subpíxel por separado (1/256 de píxel con 8 bits), así que
  esa esquina puede quedar una fracción de subpíxel fuera del borde. Un centro de píxel que cae
  en esa rendija no lo cubre ningún quad, y se ve el cielo a través del interior hueco del
  terreno. Las posiciones ya eran enteras y exactas, y la luz no influye: con la luz apagada
  salían los mismos puntos. Arreglo: en el vertex shader cada esquina se corre hacia afuera a lo
  largo de los dos bordes de su quad, en su plano, lo justo para que cada borde se corra 1/64 de
  píxel en pantalla de través, perpendicular a sí mismo (`EDGE_GROWTH_PX`). Es unas 3 veces el
  peor error de redondeo (cerca de 1/180 de píxel) y no se ve. De través y no a lo largo: la
  esquina se corre en la dirección del otro borde, y cuando los dos bordes se ven casi paralelos
  en pantalla (el suelo visto por un jugador de pie, a unos 40 grados de los ejes, o cualquier
  cara rasante) solo el seno del ángulo entre ellos cierra la rendija. Así que la corrida en cada
  dirección mide 1/64 de píxel dividido por ese seno, con un tope de 1/4 de píxel
  (`EDGE_GROWTH_LEN`) para que la punta de un quad visto casi de canto no sobresalga; ahí la
  rendija se cierra 1/4 de píxel por el seno. También se corta en un cuarto de la profundidad de
  la esquina, así una esquina pegada al ojo no lo alcanza. La primera versión medía el 1/64 de
  píxel a lo largo de cada eje y se cortaba además en 1/16 de vóxel (`EDGE_GROWTH_MAX`, ya no
  está): de frente da lo mismo, pero de pie mirando el suelo dejaba rendijas (mediciones abajo).
  Correrse t vóxeles por un eje mueve el clip t veces la columna de ese eje en `view_proj`, así
  que no hace falta otra multiplicación de matriz. Las coordenadas de textura se corren igual, y
  la textura del quad queda exactamente donde estaba; las de luz no (la luz adentro se corre
  menos de 1/4 de píxel, y solo en caras rasantes). `ViewUniform` suma `viewport` (el tamaño del
  destino en píxeles) y pasa de 336 a 352 bytes.
- **Solape y orden de dibujo.** Con el crecimiento, dos quads vecinos del mismo plano se solapan
  unos 1/32 de píxel. Un centro de píxel en esa franja lo cubren los dos, con la misma
  profundidad salvo redondeo, y con `Greater` estricto gana el que se dibuja primero. Dentro de
  un chunk el orden es fijo; entre chunks, en High, sale de los `atomicAdd` de `cull.wgsl`, que
  dependen del scheduling de la GPU. Si los dos quads tienen otro material, AO o luz, ese píxel
  puede salir de uno u otro color, y en principio cambiar entre frames o entre tiers (antes los
  dos nunca cubrían el mismo centro de píxel). Se acepta: el borde está a 1/64 de píxel de ese
  centro y cualquiera de los dos colores vale. Medido: 3 vistas renderizadas 31 veces cada una en
  High, Mid y Low dan siempre los mismos píxeles en la RTX, la 780M y llvmpipe, y High da
  exactamente lo mismo que Mid y Low en la RTX y la 780M. Si el test de tiers de `gpu_terrain`
  se vuelve inestable, el arreglo es un orden fijo en High (una posición por slot en vez de
  `atomicAdd`).
- **Por qué 1/64 de píxel.** Crecer cuesta, por dos lados: los vecinos se solapan y parte del
  solape se pinta dos veces, y la cuenta del vertex shader corre en cada esquina. En la RTX,
  estático a radio 16 sin tope (unos 2230 fps, la GPU al límite), crecer 1/16 de píxel con el
  ángulo exacto costaba 6.7 % de los fps, y 1/8 de píxel proporcional a la profundidad (una sola
  multiplicación) 5.5 %. Con 1/64 de píxel el solape ya no se nota: lo que queda es la cuenta
  (la misma cuenta con crecimiento 0 cuesta lo mismo). Con la cuenta recortada (las
  coordenadas de luz no se corren) cuesta 2.8 %: 0.448 a 0.461 ms por frame, 0.013 ms. El
  crecimiento proporcional a la profundidad se descartó porque de canto se achica en pantalla:
  para cubrir las rendijas a 2 grados necesita 1/8 de píxel de frente, y eso cuesta más que la
  cuenta exacta. Partir los quads en las uniones en T también lo arreglaba, pero suma quads.

Medido el 2026-10-07 (capturas y scripts en `bench-results/pacing-2026-10-07/visuals/`):

- Puntos de cielo encerrados por terreno, con `examples/snapshot.rs` en la 780M a 1280x720:
  superficie sin árboles 11 antes y 0 después; vista rasante 4 y 0; las dos vistas del bosque al
  mediodía de `bench-results/light-look` pierden 9 y 5 islas. Las que quedan en el bosque son
  huecos de las hojas (están igual antes y después).
- `gpu_terrain` (test nuevo `terrain_has_no_cracks_on_any_tier`): 4 vistas del terreno a
  1280x720 sobre fondo magenta. Sin el arreglo, 50 a 55 píxeles de fondo encerrados por tier en
  la RTX (Vulkan y GL), la 780M y llvmpipe; con el arreglo, 0. Cambian además algunos píxeles de
  borde entre quads, casi todos en pocos niveles: es el solape.
- Lámparas: en `gpu_terrain` la cara de la lámpara blanca da la misma imagen al mediodía, a
  medianoche y sin luz, con nivel medio 232.8 de 255 en todos los adaptadores (175.5 antes).
- Costo en GPU (`stress`, estático, overworld a radio 16, sin tope, 3 corridas alternadas por
  versión): en la RTX 2230 a 2168 fps (2.8 %, 0.013 ms por frame), casi todo del crecimiento; las
  lámparas solas, 0.9 %. En la 780M 594 a 590 fps, dentro del ruido.
- Lotes de ediciones (RTX, overworld a radio 16, `stress --edits`, 20 ediciones por corrida):
  sin tope se partían 1 y 2 lotes en dos corridas, y ahora 0 y 0. Como espera el lote entero, el
  tiempo hasta publicar sube un poco: mediana 1.7 a 1.9 ms, peor 2.6 a 3.1 ms. Frames hasta
  publicar: mediana 4 antes, 3 y 4 después; peor 5 en todas. En la 780M (600 fps) no se partía
  ninguno, ni antes ni después. Con tope 144 da lo mismo que antes: 2 frames (13.9 ms) en
  mediana y en el peor caso, 0 lotes partidos.

Revisión del mismo día (capturas y scripts en `bench-results/pacing-2026-10-07/visuals2/`):

- Grietas de pie: 288 vistas del terreno de `gpu_terrain` a 1280x720, desde 16 puntos y en 18
  direcciones, con el ojo 1.6 sobre el suelo mirando 4 grados abajo. Sin crecimiento, 904
  píxeles de fondo encerrados en 237 vistas; con 1/64 de píxel a lo largo de cada eje, 21 en 20
  vistas (los mismos en la RTX y la 780M); de través, 1. Otras 3 tandas de 288 (ojo a 1.6
  mirando al horizonte, a 4 vóxeles mirando 15 grados abajo, a 12 mirando 25 grados abajo): 35,
  14 y 5 a lo largo de los ejes; 0, 2 y 1 de través. Las 4 que quedan de través no son uniones
  en T, están en el borde contra el cielo: una es una rendija de verdad entre dos lomas (está
  igual sin crecimiento), y en las otras 3 el borde corrido tapó el píxel vecino de uno de
  cielo, que quedó encerrado. Con tope de 1/8 de píxel quedaban 5 y con 1/16, 8. Dos de esas
  vistas entraron a `CRACK_VIEWS` (`standing43` y `standing103`): sin crecimiento 3 y 17
  píxeles, a lo largo de los ejes 1 y 1 en todos los tiers y adaptadores, de través 0.
- Costo de través contra a lo largo de los ejes (`stress`, overworld a radio 16, sin tope, 3
  corridas alternadas por versión, estático y de pie mirando el suelo a 40 grados de los ejes):
  en la RTX 2164 a 2131 fps estático (-1.5 %, dentro de lo que varían las corridas) y 1886 a
  1872 de pie (-0.7 %); en la 780M 592 a 587 y 560 a 566, ruido.

### Fases

P0 (datos de bloque, máscaras, este ADR), P1 (`boxel_light`), P2 (mesher), P3 (integración en
`boxel_world`: `SkyMap`, jobs de caminata, luz en `start_mesh`, ediciones con lotes atómicos), P4
(render), P5 (día y noche) y P6 (lámparas de color, lava provisoria, overlay y herramientas) están
hechas. Siguen P7 (medir con la máquina libre y decidir) y P8.

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
- P0 y P1 no cambiaron nada en pantalla. P2 pasa los quads a v2 y el mundo ya hornea AO, que el
  shader de hoy dibuja. P3 calcula la luz en el mundo (las caras oscuras pierden su AO); la luz se
  ve con P4 (el shader la lee).
- Riesgos abiertos: Low reserva un segundo buffer del tamaño del arena (64 MiB por defecto),
  ocupado o no; una paleta con más de 4096
  entradas pierde precisión en los canales de bloque; las ediciones de luz remallan hasta 8
  chunks, más las bandas de los techos que se mueven; un viewer cuya vista toca los 1024 vóxeles
  bajo el tope camina las columnas generando la superficie (abajo). El AO en la clave del merge
  pasó el gate G1 (abajo).

**P2 (mesher), medido el 2026-10-06** con criterion (release), cada bench después de
`tools/wait-idle.sh`, en la máquina compartida con otros builds (load average de 3 a 7): números
indicativos, se repiten en P7.

| Bench | Medido | Objetivo |
|---|---|---|
| `mesh/cave` (AO apagado, sin luz: el camino de bgm) | 19.6 µs | 1.15x de 28.8 µs o menos |
| `gather_mesh/cave` (luz uniforme oscura, AO solo en caras con luz) | 82.4 µs | 1.1x de 91.5 µs o menos |
| `gather_mesh/cave_lit` (146 lámparas, luz calculada antes) | 103 µs | 160 µs o menos |
| `gather_mesh/surface` (luz de cielo y claves de AO, luz calculada antes) | 143 µs | 150 µs o menos |

En la superficie (2993 caras visibles), solo el paso de malla: 19 µs sin AO ni luz, 33 µs con AO
en todas las caras y 82 µs con la luz. La luz cuesta sobre todo el promedio de las esquinas (cada
celda se expande una vez a carriles de 16 bits y las 4 esquinas se suman en paralelo) y la
búsqueda de la clave.

Quads de `headless_world` a radio 16 (sin luz todavía: con `lit-only` toda cara tiene luz, así
que da lo mismo que `baked`):

| Mundo | `--ao off` | `--ao baked` y `lit-only` |
|---|---|---|
| overworld, y = 80 | 4 222 233 | 7 330 180 (1.74x) |
| overworld, y = -1000 | 8 407 250 | 14 669 107 (1.74x) |
| forest, y = 80 | 439 764 | 717 965 (1.63x) |

- Hasta P3 el mundo paga el AO en todas las caras, cuevas incluidas: bajo tierra a radio 16 son
  14.7 M quads en lugar de 8.4 M (con la luz de P3, 8.4 M: abajo). Con la luz de P3 las cuevas oscuras vuelven a fusionarse como
  con AO apagado (estimado 1.0 a 1.05x); el gate G1 de P3 lo mide.
- Las paletas son de 1 entrada mientras la luz sea uniforme. En los benches: 74 entradas en la
  superficie con árboles y 411 en la cueva con lámparas.
- El fork de bgm fusiona una cara a la vez y saca los quads ya en forma canónica (sin pasar por
  los vértices de bgm): `mesh/cave` con AO apagado da 19.6 µs contra los 28.8 µs medidos antes.
- Memoria: el `Mesher` de cada hilo pasa de unos 210 KiB a unos 0.6 MiB (claves de 34³ en `u32`,
  la paleta y la tabla de claves).

**P3 (mundo), medido el 2026-10-06** con `headless_world` (release, `--hz 0`). Los quads no
dependen de la carga de la máquina. Gate G1, quads contra hoy (luz apagada y AO apagado), con luz
y AO `lit-only` (el valor por defecto):

| Vista | Hoy | Con luz | Relación | Límite |
|---|---|---|---|---|
| forest, radio 16, y = 80 | 439 764 | 766 460 | 1.74x | 1.9x |
| overworld, radio 16, y = 80 | 4 222 233 | 4 562 418 | 1.08x | 1.25x |
| overworld, radio 16, y = -1000 | 8 407 250 | 8 407 250 | 1.00x | 1.10x |
| overworld, radio 32, y = 80 | 16 690 909 | 18 043 891 | 1.08x | 22 M |
| overworld, radio 32, y = -1000 | 33 613 075 | 33 613 075 | 1.00x | |
| forest, radio 32, y = 80 | 1 760 326 | 3 076 455 | 1.75x | |

El forest a y = -1000 no tiene nada (está debajo de su fondo). G1 pasa: queda `BakedLitOnly`.

- AO omitido en caras oscuras contra AO en todas (`--ao baked`), con luz, a radio 16: overworld
  y = 80, 4.56 M contra 7.38 M; y = -1000, 8.41 M contra 14.67 M; forest igual (todo tiene luz).
  Sin AO (`--ao off`) la luz sola cuesta 1.03x en el overworld (las claves de luz parten algunas
  fusiones) y 1.27x en el forest (las sombras de los árboles).
- Caminos de la luz y caminatas a radio 16: overworld y = 80, 1690 cajas con inundación y 5266
  rápidas, 933 columnas caminadas con 1 generación privada; forest, 1645 y 1934, 933 caminatas,
  ninguna privada. A y = -1000 todas las cajas son rápidas, pero las 6 capas más altas de la
  vista quedan a menos de 1024 vóxeles del tope del generador, así que las 933 columnas se
  caminan y generan 2540 chunks de superficie que no están cargados (unos 2.7 por columna).
- Memoria: la caché de luz de gameplay, 4.6 MiB (radio 8); los tiles, 7.4 MiB a radio 16 y
  27.7 MiB a radio 32. Paletas: p50 1, p99 38, máximo 222, ninguna recuantizada.
- P3 no suma benches de criterion: el trabajo nuevo de la luz son las funciones medidas en P1.

Cambios de API que rompen:

- `PackedQuad`: se van `with_light`, `sky` y `block_light`; entran `with_light_index`,
  `light_index`, `with_flags`, `flags`, `with_ao_byte` y `ao_byte`. En `layout` se van
  `MAT_SKY_SHIFT`, `MAT_BLOCK_SHIFT` y `MAT_FLAGS_SHIFT`; entran `GEO_FLAGS_SHIFT`,
  `MAT_LIGHT_SHIFT` y `MAT_LIGHT_MASK` (también en `common.wgsl`).
- `ChunkMesh` suma `palette`, `light_quantization` y `hash` (`ChunkMesh::from_quads` arma una a
  mano con la paleta de cielo abierto). `MeshInput` suma `light`; `Mesher::mesh` y
  `Mesher::mesh_chunk` toman la luz y `MeshOptions`. `WorldSettings` suma `ao_policy`.
- `boxel_mesh` depende de `boxel_light`; `binary-greedy-meshing` queda solo como dependencia de
  desarrollo.
- `BlockDef.light_emission` desaparece: `emission: [u8; 3]` y `light_opacity: u8`.
  `BlockRegistry::light_emission` pasa a `emission` (devuelve `LightValue`) y `light_opacity`.
  Nuevos: `RegistryError::InvalidLight` y `BlockRegistryBuilder::with_default_blocks`.
- `ChunkData.blocks` es privado (`blocks()`, `blocks_mut()`, `from_paletted()`, `compact()`).
  `ChunkData.light` y `LightStorage` desaparecen. Nuevos: `ChunkData::light_masks`, `may_emit`,
  `has_cached_light_masks`, los tipos `LightMasks`, `MaskRows` y `OwnedLightMasks`, y
  `heap_bytes` cuenta las máscaras construidas.
- Nuevo crate `boxel_light` (depende solo de `boxel_voxel`).
- P3: `WorldSettings` suma `light`, `sky_walk_limit`, `light_cache_radius` y
  `edit_batch_hold_frames` (desde la sección 12, `edit_batch_hold`); `WorldContent` suma
  `light_tables` y `WorldContent::new`; `WorldStats` suma `light_full`, `light_fast`,
  `sky_columns`, `sky_walks`, `sky_private_generations`, `waiting_sky`, `edit_batches_held`,
  `edit_batches_split` y `light_fanout`. Nuevos: el recurso `SkyMap`, `ColumnId`,
  `ChunkMap::light_at`, `ChunkSlot::light` y el módulo `reference`. En `scheduler`,
  `JobKind::SkyTile` y los jobs se identifican con `JobId` (`Chunk` o `Column`) en lugar de
  `ChunkId`. `boxel_light` suma `LayerData::AirUntil`. `boxel_world` depende de `boxel_light`.
  Una malla igual a la publicada ya no genera un `ChunkMeshEvent::Upsert`.
- P4: `RendererConfig::clear_color` desaparece; `ViewParams` suma `clear_color` y `light`
  (`SceneLight`), y `ViewParams::from_camera` toma el color del cielo para el fondo y la niebla.
  Nuevos: `SceneLight`, `light_curve`, `LIGHT_LEVELS`, `DEFAULT_SKY_COLOR` y `arena_len` en
  `boxel_render`; `RenderSettings` suma `ao_strength`, `block_light_gain` y `light_gamma`.
  `RenderStats::arena_used_quads` cuenta quads más paletas. `ChunkGpu` cambia `pad: [u32; 3]`
  por `light_base` y `pad: [u32; 2]`; `ViewUniform` suma `sky_light`, `light_params` y
  `light_curve`. `floors::VERTEX_BUFFERS` pasa a 2: un adaptador con un solo vertex buffer ya no
  entra en ningún tier. En WGSL, `decode_quad` toma la entrada de paleta y `VsOut` cambia `shade`
  por `face`, `st`, `light` y `ao`.
- Revisión de P2 a P6: `WorldStats` suma `light_jobs`; `ChunkSlot::light` devuelve
  `Option<&ChunkLight>`; `scheduler` suma `JobKind::Light`; `ChunkGpu` cambia `pad: [u32; 2]`
  por `light_len` y `pad`; `light_cache_radius` se limita a 32. `TimeOfDay::set_clock` toma el
  primer tick en la hora pedida o después, y `clock_text` cuenta minutos enteros, así
  `--time HH:MM` muestra `HH:MM`.
- Arreglos de aspecto (sección 12): `layout::FLAG_EMISSIVE`, `PackedQuad::is_emissive`,
  `MaterialTable::emissive` y `MaterialTable::set_emissive`; `WorldSettings::edit_batch_hold`
  (`Duration`) y `WorldSettings::DEFAULT_EDIT_BATCH_HOLD` reemplazan a `edit_batch_hold_frames`;
  `ViewUniform` suma `viewport` (352 bytes). En WGSL, `VsOut::ao` lleva también los flags (bits 8
  a 12), y `Decoded` suma `flags` y `grow`.
- P5: nuevos en `boxel_world`: `TimeOfDay`, `DaylightCurve`, `Daylight`, `SkyKey`,
  `DaylightPlugin` (en `DefaultPlugins`) y las constantes `DEFAULT_TICKS_PER_DAY`,
  `TICKS_PER_SECOND` y `SUNRISE_HOUR`.
- P6: `BlockRegistry::default_blocks` pasa de 12 a 17 estados y de 13 a 18 texturas, y el
  registry de fábrica ya tiene emisores (`LightTables::any_emitter` es verdadero). Nuevos:
  `boxel_voxel::{BuiltinLamp, LAMPS, LAVA_EMISSION}`, `boxel_debug::{CrosshairTarget,
  LightProbe}` y los campos `light` y `time` de `OverlayInputs`.

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
- `boxel_mesh/tests/oracle.rs`: con luz aleatoria (ausente, uniforme, suave con luces de color,
  en bloques, en banda y ruido) y cada política de AO, cada quad expandido a caras unitarias y
  leído por la paleta es igual a `reference_face_shading` (al nivel de cuantización de la malla);
  volver a mallar con un `Mesher` usado da la misma malla, hash incluido. La recuantización
  forzada: una banda de luz cae en `Block3` y el ruido en `Flat`.
- `boxel_mesh/tests/bgm_regression.rs`: con AO apagado y sin luz (o luz uniforme, o luz oscura
  con `BakedLitOnly`) los quads son idénticos byte a byte a los de bgm 0.5.2.
- `boxel_mesh/tests/mesher.rs`: el AO de un piso junto a una pared sigue la política, las hojas no
  dan AO, la luz parte las fusiones y la paleta es determinista. En `boxel_world`, una edición en
  un borde remalla también el vecino de arista (`padded_neighbors`, contra fuerza bruta).
- `boxel_light/tests/sky_walk.rs`: la caminata igual a un recorrido de fuerza bruta de los chunks
  generados (overworld con árboles y entradas, forest, terrain y flat, cerca y en 2^40), el
  límite, el vacío, la búsqueda después de una edición, una edición 5000 vóxeles arriba que solo
  cambia su columna (la edición aplicada al tile es igual a una caminata nueva), tiles sin piso,
  generadores sin tope y `AirUntil` (una plataforma a 2^50 se camina en pocas capas).
- `boxel_world/src/tests/light.rs` (P3, mesher real; cada caso compara las mallas con
  `Reference`): las mallas no dependen del camino del viewer (volar, bajar, teletransportarse),
  del ejecutor ni de los hilos (1 y 6); un mundo en reposo malla cada chunk una vez; ediciones
  al azar dejan cada malla igual a la de cero y todo chunk que cambió fue remallado; quitar una
  lámpara es igual a no ponerla; cavar en una cueva oscura remalla solo su chunk, y ahí abajo no
  se camina ninguna columna; un pozo tiene cielo 15 en el fondo y queda oscuro al taparlo; el
  límite de la caminata corta un pozo más hondo; una plataforma a +5000 sombrea el suelo y al
  descargarse se lleva la sombra; contenido debajo del piso cierra en el piso las columnas
  abiertas (y al quitarlo vuelven a abrirse); un lote de ediciones sale en un frame, o en partes
  pasado `edit_batch_hold` y 2 frames (con un reloj manual: frames de 1, 6.9, 10, 17 y 33 ms;
  con espera 0 nada se retiene); una segunda edición de un chunk retenido sale en el mismo frame que la primera; las
  ediciones pasan delante de una cola de mallas del streaming; la caché de luz sigue a un viewer
  que avanza 17 chunks (y cavar allá remalla solo el chunk); los chunks de puro aire cerca del
  viewer guardan su luz; el mismo mundo en x = 2^40, y = -2^40 da los mismos bits.
- `boxel_world/src/streaming/tests/sky.rs`: las caminatas esperan los datos cargados y no
  generan nada; una caminata que leyó contenido editado se repite; un chunk editado que se
  descarga devuelve su columna a una caminata; generadores sin tope o vacíos; viewers a 2^35
  capas en las mismas columnas, con bloques bajo el piso y muy arriba, se asientan enseguida.
  En `streaming/tests.rs`, un remallado fallido no deja luz vieja como actual, los jobs de un
  lote siguen en prioridad 0 cuando el viewer se mueve, un resultado retenido que se va del lote
  se lleva el reloj de la espera, y un lote sin resultados se cierra pasados 120 frames y 1 s
  (con frames de 1 y 17 ms). En `sky.rs`, las
  reglas según el rango del generador, `padded_chunks` contra fuerza bruta y las caminatas con
  capas capturadas, el vacío y el contenido fuera del generador.
- `boxel_world/tests/integration.rs`: cada malla del terreno real, también después de editar,
  igual a la de `Reference`; `light_at` lee la caché.
- `boxel_shaders/tests/naga.rs` (P4): las constantes de `LightValue` en WGSL, `View` de 336 B
  (352 desde la sección 12, con `FLAG_EMISSIVE` igual al de `boxel_mesh`),
  `light_base` en el offset 52, Low con dos atributos de instancia `Uint32x2` y un solo uniform
  block; High y Mid con un atributo. `boxel_gfx/tests/policy.rs`: todo tier pide 2 vertex
  buffers y con 1 no clasifica.
- `boxel_render/tests/gpu_terrain.rs` (P4 y P5): una escena iluminada hecha a mano (árbol,
  alero, pared baja junto a suelo con sol, cueva cerrada con lámparas roja, azul y naranja) con la
  luz calculada como la calcula el mundo, en 7 vistas (mediodía y medianoche). Los píxeles son
  idénticos en High, Mid y Low y en cada posición de `FAR_SHIFTS`, donde las mallas salen iguales
  byte a byte; la cueva es igual al mediodía y a medianoche; la noche tiene como mucho la mitad
  del brillo del día; la cueva tiene píxeles rojos y azules; bajo el árbol, el alero y junto a la
  pared la imagen es más oscura que sin luz ni AO. Desde la sección 12, 9 vistas: la cara de la
  lámpara blanca brilla igual al mediodía, a medianoche y sin luz; y un tercer test busca grietas
  en 6 vistas del terreno a 1280x720, 2 de ellas de pie mirando el suelo de costado (ninguna en
  ningún tier). `boxel_mesh/tests/mesher.rs`: los
  quads de cada emisor, y solo esos, llevan `FLAG_EMISSIVE`.
- `boxel_render` (unitarios): el smoke test de cada tier dibuja un quad opaco con luz azul
  (entrada 0) bajo un quad cutout más chico con luz roja (entrada 1), y falla si el centro no
  sale rojo o una esquina no sale azul (así cada quad recibe su entrada también en GL, donde el
  primer instance del draw cutout se emula por buffer); `gpu_tiers` comprueba que un índice
  fuera de la paleta da cielo abierto en cada tier;
  la paleta va detrás de los quads en el arena y en Low cada quad recibe su entrada; la escena
  sigue al reloj del mundo. `gpu_tiers` cuenta las entradas de paleta en el arena lleno.
- `boxel_world/src/daylight.rs` (P5): un día son 76 800 pasos fijos, los ticks son
  deterministas (pausa y velocidad incluidas), mover la hora da la vuelta dentro del día y la
  curva da 1.0 blanco al mediodía, 0.2 azul a medianoche, 0.6 cálido en el horizonte y 0.02 de
  ambiente a toda hora.
- P6: el registry de fábrica fija los estados 12 a 16 y sus emisiones (`contract.rs`,
  `masks.rs`); el overlay muestra la luz del ojo y de la mira y la hora; `voxel_world` y `stress`
  prueban `--time`, `--lamps` y el guion de `--edits`. La app sin ventana de `voxel_world`
  (`Update` estricto, con `DaylightPlugin`) prueba las teclas del reloj junto a G y los dígitos,
  la celda de `CrosshairTarget` al apuntar y las lámparas de `--lamps` sobre el suelo;
  `tests/ambiguity.rs` suma `DaylightPlugin` y pone estricto `FixedUpdate`.
- `BlockChanged`: el harness de `streaming/tests.rs` compara cada llamada a `apply_edits` con
  una diferencia del mapa (también con chunks en cola o generándose, y en el proptest de
  `tall.rs`), y `boxel_world/tests/messages.rs` muestra que un lector en un schedule fijo recibe
  todos los mensajes, aunque el mapa ya tenga otra edición.

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
  (sección 8), porque no puede leer la paleta de un storage buffer.
- **Mezcla aditiva de colores**: necesita un BFS por fuente y satura.

### 13. Formas de bloque (2026-10-07, ADR 0018)

- Opacidad de luz: vidrio y plantas 0, losas 1 (no tapan del todo; un valor de 2 a 14 llevaría
  la inundación a su camino lento en todos los chunks). Solo un cubo `Opaque` exige 15.
- Una cara de losa usa la misma regla de luz y AO que la cara del cubo en su lugar (las 9 celdas
  de la capa de enfrente), y el shader mueve `t` con la altura real: la media cara se ve igual
  que esa parte de la cara del cubo. Una cruz toma la luz de su propia celda, sin AO.
