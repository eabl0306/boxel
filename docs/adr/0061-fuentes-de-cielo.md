# ADR 0061: fuentes de cielo y ambiente por reino

- Estado: aceptado (issue #140, que reemplaza a #130). El dueño aprobó el plan
  (`docs/planes/sky-sources-plan.md`) el 2026-11-02, con la opción A y sus respuestas a las
  preguntas abiertas (sección 9 del plan). Se construye por carriles (sección 8 del plan). Cada
  carril que toca una zona protegida pide su OK cuando le toca. Hechos: el carril 0 (este ADR) y
  el 1 (los estados de fuente en `boxel_voxel`).
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

## Medidas

- Carril 1, criterion `masks`, una corrida por lado contra a9a0abf, una detrás de la otra:
  `cave_1bit` 4,52 a 4,50 ns (sin cambio), `surface_packed` 12,09 a 12,21 µs (+0,9 %, dentro del
  ruido). Nuevo: `cave_with_sources` (la cueva de 1 bit con 434 fuentes bajo sus techos, 2 bits)
  16,8 µs para construir máscaras y lista.

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
- Un tile de rayos cuesta 2 KiB más 8 B por fuente. Un techo lleno de fuentes en toda la vista
  son unos 11 MiB a r16.
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
