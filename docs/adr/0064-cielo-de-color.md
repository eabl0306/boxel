# ADR 0064: cielo de color a través del vidrio

- Estado: aceptado (issue #11, fase P8 del plan de luz). El dueño aprobó el plan
  (`docs/planes/colored-sky-plan.md`) el 2026-11-02, con sus respuestas a las preguntas abiertas
  (sección 11 del plan). Se construye por carriles (sección 7 del plan). Hechos: el C0 (este ADR)
  y el C1 (la luz, en `boxel_light`). Faltan el C2 (mundo), el C3 (malla), el C4 (render y
  shaders) y el C5 (depuración, demo y herramientas).
- Fecha: 2026-11-02

## Contexto

ADR 0060 hizo que el vidrio de color tiña la luz de bloque, pero dejó el cielo blanco: de día, un
cuarto detrás de un vitral se ve con luz blanca. El dueño quiere que el cielo también se tiña.

ADR 0060 daba tres razones en contra. Así quedan:

| Razón | Respuesta |
|---|---|
| Más bits por vóxel duplican `PaddedLight`, `ChunkLight`, la paleta y el formato de la GPU en todos los tiers | Solo los jobs y chunks donde un filtro cambió el color del cielo usan la forma ancha. El resto queda como hoy. La entrada de la paleta sigue en 8 bytes |
| Pasar el cielo filtrado a la luz de bloque deja de seguir el día | El tinte queda en canales de cielo, que pasan por el color del cielo y la luz del día en el shader, como hoy |
| Oscurecer el cielo sin color hace que el vidrio cuente para el techo de cielo | El techo `T` no cambia. Los filtros sobre `T` van en una lista aparte del tile |

## Decisión

### 1. La regla

El cielo se filtra igual que la luz de bloque (ADR 0060), canal por canal.

- El cielo tiene tres canales, `S_r`, `S_g` y `S_b`, de 0 a 15.
- **Cielo esparcido**: al entrar a un vóxel con filtro, cada canal vale `transmit(l - costo, f)`.
  Los demás vóxeles cuestan `max(1, opacidad)`, como siempre.
- **Cielo directo**: un vóxel sobre `T` es directo, como hoy. Su nivel por canal es 15 pasado por
  cada filtro que tiene encima en su columna, el suyo incluido, sin costo (el cielo directo no
  baja con la profundidad). Todos esos filtros están sobre `T`, así que tienen opacidad 0.
- Un vóxel directo con 15 en un canal está cerrado a la inundación de ese canal, como hoy. Uno
  teñido es una fuente a su nivel y queda abierto: un vecino más brillante lo puede subir (el
  borde de un techo de vidrio recibe luz blanca de la columna abierta de al lado).
- Los rayos de las fuentes de cielo (ADR 0061) siguen la misma regla: 15 por canal desde la
  fuente, filtrados por cada celda con filtro en su dirección. Se mezclan con el cielo directo
  de la columna por el máximo de cada canal.
- Los canales se mezclan por el máximo. `T` no cambia: la caminata, el piso, los caminos rápidos
  y las fuentes de cielo siguen igual.

Por qué el cielo esparcido también se tiñe: el caso principal es un vitral en una pared, y por ahí
no entra cielo directo (el cielo directo es vertical). Un cuarto con techo solo recibe cielo
esparcido por sus ventanas.

Sigue siendo exacta y única: `transmit` nunca sube un nivel y es monótona (ADR 0060), y las fuentes
teñidas valen 15 o menos. Toda luz del chunk acolchado sigue viniendo de una fuente a 14 vóxeles
o menos, dentro de la caja. Los tintes del cielo directo salen de la columna, que el tile calcula
desde el contenido. El punto fijo es único y la cola por niveles saca cada vóxel en su nivel
final.

Lo que da con los colores del juego:

| Vidrio | Filtro | Cielo directo debajo | Cielo esparcido detrás (entra 14) |
|---|---|---|---|
| Rojo | `[10, 3, 2]` | `(13, 7, 6)` | `(12, 6, 5)` |
| Celeste | `[3, 11, 13]` | `(7, 14, 14)` | `(6, 13, 13)` |
| Negro | `[2, 2, 2]` | `(6, 6, 6)` | `(5, 5, 5)` |
| Rojo y después celeste | | `(5, 6, 5)` | |
| Común | ninguno | `(15, 15, 15)`, como hoy | como hoy |

### 2. Opción D: cielo de color ralo

Las opciones, con detalle en la sección 3 del plan:

| | A: RGB en todo | B: índice de tinte | C: solo cielo directo | **D: ralo (elegida)** |
|---|---|---|---|---|
| `PaddedLight` por job | 154 KiB siempre | 96 KiB siempre | 77 KiB | 77, y 154 solo los teñidos |
| Entrada de paleta | 16 B en todo | 16 B y tabla de tintes | necesita la de D | 8 B en todo |
| VRAM fija de Low | 64 a 128 MiB | 64 a 128 MiB | como D | sin cambio |
| Luz exacta | sí | no (dos caminos no se mezclan) | no (las ventanas no dan nada) | sí |
| Sin vidrio, gratis e idéntico | no | no | sí | sí |

D calcula la regla exacta y guarda la forma ancha solo donde se ve el color. Un mundo sin vidrio
de color queda idéntico byte a byte y no paga nada.

### 3. Decisiones del dueño

1. **D en Low y Mid, caras exactas solo en High.** Low y Mid guardan un tinte por cara cuando una
   cara tiene a la vez cielo teñido y luz de bloque que varían entre sus esquinas (sección 8).
   High dibuja esas caras exactas, con D' o algo equivalente que no pida un buffer más en Low.
   Es una diferencia solo de High, como las sombras del sol (#9): las pruebas de píxeles comparan
   Low con Mid, y High con su propia referencia, en escenas con caras así. Las escenas sin esas
   caras siguen iguales en los tres tiers.
2. **El gameplay lee el cielo sin teñir.** Los cultivos (y después la aparición de criaturas) ven
   el nivel de cielo que tendría el vóxel si el vidrio no tiñera. Un invernadero de cualquier
   vidrio funciona como en Minecraft. Solo la imagen se tiñe.
3. **Apagado por defecto.** El motor trae `WorldSettings::colored_sky` apagado; el juego lo
   prende.
4. **Un solo filtro para las dos luces.** No hay forma de que un bloque filtre la luz de bloque y
   no la del cielo. El juego no le pone filtro al agua.

### 4. Tiles: los tintes sobre el techo

- `SkyTile` guarda, por columna, los filtros sobre `T` (`Tint`: altura y filtro), del más alto al
  más bajo: `SkyTile::tints`, `tint_max` y `tint_count`. Un tile sin vidrio sobre sus techos no
  guarda nada (`None`).
- Solo un tile caminado con `sky_tile::walk_colored` (o armado a mano con `with_colored_sky` o
  `with_tints`) guarda tintes. `walk` da el tile de antes, sin tintes, así que con el interruptor
  apagado los tiles son los de siempre.
- La caminata lee la máscara de filtros de una capa solo si no está vacía, y anota los filtros de
  opacidad 0 que encuentra mientras su columna sigue abierta y sobre el piso. Una capa uniforme de
  vidrio también cuenta.
- Ediciones: `raise` quita los tintes que quedan en o bajo el techo nuevo; `rescan_column` junta
  los filtros entre el techo nuevo y el viejo, el vóxel del techo viejo incluido (pudo volverse
  vidrio); `set_filter(x, z, y, filtro)` pone, cambia o quita uno sobre `T`. Un tile editado queda
  igual a una caminata nueva del contenido editado.

### 5. La caja

- **Interruptor**: `LightInput::colored_sky`. Apagado, ninguna caja toma el camino de color y el
  resultado es el de antes bit a bit.
- **Compuerta**: el camino de color corre solo si el interruptor está prendido, el registro tiene
  filtros, la caja tiene cielo mixto, y además algún chunk de los 27 tiene vóxeles con filtro, o
  algún tile tiene un tinte en o sobre el fondo de la caja, o algún rayo con filtro cruza la caja.
  El resto corre el código de antes.
- **`FastOpen`** pide además que ningún tinte de los 9 tiles esté en o sobre el fondo del chunk
  acolchado: un chunk bajo un techo de vidrio es directo pero no blanco.
- **Cielo directo**: una pasada de arriba abajo por la caja guarda el tinte de cada columna y lo
  cambia en cada tinte que cruza. Los vóxeles directos bajo un tinte toman su nivel por canal en
  una segunda caja (`rgb`, 512 KiB) y quedan abiertos. Los rayos blancos siguen blancos; los
  rayos con filtro escriben sus celdas con el máximo por canal.
- **Inundaciones**: la del cielo sin teñir corre como siempre (es la que lee el gameplay). Después,
  tres más, una por canal, sobre `rgb`, que pagan cada vóxel con filtro uno por uno. Un vóxel
  teñido se encola solo si un vecino lo puede necesitar: junto a un vóxel abierto que no es
  directo, o en una columna cuyos tintes difieren de los de una vecina (o con un rayo teñido
  cerca). Debajo de un techo parejo, el interior teñido no se encola.
- **Salida**: si algún vóxel acolchado tiene canales de cielo distintos de su cielo sin teñir,
  `PaddedLight::Colored`; si no, el `Full` o `Uniform` de siempre.

### 6. Salidas

- `PaddedLight::Colored { light, sky_rgb }`: `light` es el `Full` de siempre (cielo sin teñir y
  luz de bloque); `sky_rgb` lleva el rojo, verde y azul del cielo en los tres nibbles de bloque de
  un `LightValue`. 154 KiB, solo en jobs teñidos.
- `ChunkLight::Colored { light, sky_rgb }`, 128 KiB, solo en chunks teñidos dentro de
  `light_cache_radius`. Si el tinte queda solo en el borde acolchado, el chunk toma su forma de
  siempre.
- `get` da la luz con el cielo sin teñir en todas las formas: el gameplay, los ticks (ADR 0059),
  F3 y el alcance de las ediciones leen lo de siempre. `sky_rgb` da los tres canales (el nivel sin
  teñir tres veces en las otras formas).
- Un vidrio gris o negro solo oscurece, pero igual pide la forma ancha: la imagen ve 6 y el
  gameplay 15.

### 7. Rayos de color

- `beam_tile::build_colored` guarda las celdas con filtro (opacidad 0) dentro de los rayos de
  cada columna (8 B por celda) y marca los rayos que cruzan alguna (`FLAG_TINTED`). `build` da el
  tile de antes.
- `BeamTile::edit` en un tile de color vuelve a leer las celdas de los rayos nuevos de la columna,
  así un vidrio puesto en un rayo lo retiñe en el lugar. `Retrace::Changed` cubre también las
  celdas cuyo color cambió.
- Una fuente no puede tener filtro (`set_sky_source` lo rechaza), así que todo rayo sale blanco.

### 8. Malla y GPU (carriles C3 y C4, por construir)

- La entrada de la paleta sigue en `[u32; 2]`. Un chunk teñido tiene entradas normales (las de
  hoy) y teñidas: 4 esquinas de un tipo de luz (cielo o bloque, 3 canales), el otro tipo plano para
  la cara y un bit que dice cuál. Se guarda con esquinas el tipo que más varía. Es exacta cuando
  uno de los dos tipos es parejo en la cara.
- Las teñidas van primero; `ChunkMesh::tinted_entries` dice dónde empiezan las normales. High y
  Mid leen la cuenta de `ChunkGpu.flags` (bits 7 a 19); Low de `ChunkImmediate.origin.w`. Un
  chunk sin entradas teñidas queda igual byte a byte.
- High dibuja exactas las caras con los dos tipos variando (decisión 1); Low y Mid las aplanan.
- El fragment necesita un bit plano por quad que diga si la entrada es teñida. El plan lo ponía en
  el bit 16 de `VsOut::ao`, pero ya no está libre: los bits 16 a 31 son la máscara de luces
  dinámicas (ADR 0062, `AO_DYNAMIC_SHIFT`). El C4 elige otro lugar: un varying plano nuevo, o 15
  luces dinámicas en vez de 16.
- El término del cielo de `voxel_light` también va a cambiar con las sombras del sol (plan de
  sombras, carril S1, un factor `sun_factor`), que se espera antes del C4: el C4 multiplica cada
  canal del cielo teñido por ese mismo factor.
- Celdas (`CellVertex`, 40 a 44 B) y modelos (`ModelVertex`, 32 a 36 B) llevan el cielo RGB de su
  celda.

### 9. Carril C1: cómo quedó

Lo que cambió respecto del plan:

- **Los 16 bits bajos llevan el cielo sin teñir**, no el canal más brillante. Lo pide la decisión
  2: así el gameplay, los ticks y todo lector de hoy siguen sin cambios. Por eso hace falta una
  cuarta inundación en las cajas teñidas, y un vidrio gris o negro también da la forma ancha.
- **Dos arreglos de `u16` en lugar de uno de `u32`** en `PaddedLight::Colored` y
  `ChunkLight::Colored`. Ocupan lo mismo, y el mesher tiene la vista de 16 bits sin copiar nada
  (el C3 lee las dos vistas igual). Hasta el C3, el mesher usa `light`: la imagen sale sin teñir.
- **Caminata y rayos aparte**: `walk_colored` y `build_colored`, para que con el interruptor
  apagado los tiles sean los de antes byte a byte. El mundo todavía llama a `walk` y `build`: el
  C2 elige la versión de color cuando `colored_sky` está prendido.
- **El índice de tintes usa `u32`** (4 KiB por tile con tintes), no `u16`: 32 capas de vidrio sobre
  una columna de chunks pasan de 65 535 tintes.
- **El scratch de color ocupa unos 0,7 MiB por hilo** (no 0,25 a 0,5): la caja `rgb`, cuatro
  máscaras y una tabla por columna. Se crea en la primera caja teñida.
- `LightPath::Full` suma `colored_pops`: `Some` con los vóxeles de las tres inundaciones si la caja
  tomó el camino de color. El C2 lo usa para sus contadores.
- `WorldSettings::colored_sky` (apagado) llega a cada job de luz por `SkyMap`. No hace nada más
  hasta el C2.
- El oráculo tiene `OracleWorld::solve_colored`, con los tres canales del cielo.
- Las luces puntuales horneadas (ADR 0063) son luz de bloque: en la forma de color suben los
  canales de bloque de `light` y no tocan `sky_rgb`. Detrás de un vidrio se filtran igual que una
  lámpara. Una prueba lo fija en mundos con vidrio y `colored_sky` prendido.

Para el C2: el gameplay ya lee el cielo sin teñir. `ChunkMap::light_at`, `TickContext::light` y
`brightness` leen `ChunkLight::get`, que lo da en todas las formas: no hay que cambiar nada ahí.
Para la imagen y F3 está `ChunkLight::sky_rgb` (el futuro `ChunkMap::sky_rgb_at`).

## Medidas

Carril C1, criterion `light` y `sky_tile`, una corrida por lado contra main (47013e0), una detrás
de la otra, con la máquina quieta. Sin vidrio nada cambia:

| Bench | main | rama | Diferencia |
|---|---:|---:|---|
| `light/box_cave_dark` (camino rápido) | 41,3 ns | 42,5 ns | +2,8 %, ruido |
| `light/box_surface` | 42,2 µs | 40,7 µs | -3,4 %, ruido |
| `light/box_surface_cold_masks_filters` | 150,8 µs | 139,1 µs | -7,8 %, ruido |
| `sky_tile/walk_loaded` | 16,6 µs | 16,7 µs | +0,9 %, ruido |

Nuevos, solo en la rama, con `colored_sky` prendido:

| Bench | Tiempo | Qué mide |
|---|---:|---|
| `light/box_glass_roof` | 431 µs | `box_surface` bajo un techo de vidrio rojo sobre el chunk y 8 vóxeles alrededor, a 30 vóxeles del suelo (sobre los árboles) y abierto por los cuatro lados: 33 899 pops en las tres inundaciones de color, contra 2 760 de la de siempre |
| `light/box_glass_roof_clear` | 40,6 µs | el mismo techo de vidrio común: el camino de siempre |
| `light/box_window` | 163 µs | un cuarto de piedra cerrado, con luz solo por una ventana azul de 8 por 16 |
| `light/box_window_clear` | 48,1 µs | la misma ventana de vidrio común |
| `sky_tile/walk_loaded_glass` | 38,8 µs | `walk_loaded` con un techo rojo sobre toda la columna: 1 024 tintes, 37 KiB de tile |

- **`box_glass_roof` no llega a la meta del plan (150 µs).** El costo es luz de verdad, no trabajo
  de más: bajo un techo alto y abierto por los lados, el cielo blanco entra por los cuatro
  costados y se esparce de 8 a 14 vóxeles en verde y azul, en toda la altura. La de siempre no
  inunda nada ahí, porque todo es directo. Un invernadero cerrado por los lados cuesta como
  `box_window`. Solo pagan las cajas cerca de un techo de vidrio con cielo. Si el C5 (`glass-house`)
  lo muestra caro, los próximos pasos son: saltar los canales que ningún filtro de la caja baja
  (el plan, sección 4.2), y un camino sin `state_at` para los vóxeles sin filtro.
- `walk_loaded_glass` no llega a la meta de 1,1 veces: las máscaras de las capas con vidrio
  suman el pase de filtros (ADR 0060) y cada tinte lee su estado. Pasa una vez por columna y
  edición, no por caja.

## El juego

- Nada nuevo que registrar: los 16 vidrios de color y sus paneles ya tienen el filtro de ADR 0060,
  y el cielo lo sigue.
- El juego prende `colored_sky`.
- No le pone filtro al agua ni a las hojas: cada océano o bosque tomaría el camino de color.

## Consecuencias

- De día, un cuarto detrás de un vitral se ve del color del vidrio, y de noche se oscurece como el
  cielo abierto. Las lámparas siguen igual.
- Los cultivos bajo cualquier vidrio crecen como bajo el cielo: el gameplay no ve el tinte.
- Una caja teñida corre cuatro inundaciones de cielo en vez de una. Un techo de vidrio grande o
  alto vuelve teñidas todas las cajas entre el techo y el suelo (más 16), y sus chunks ocupan
  128 KiB en la caché de luz. Lo acota lo que construyen los jugadores; el caso `glass-house` del
  C5 lo mide.
- Un mundo sin vidrio de color, o con el interruptor apagado, queda idéntico byte a byte.
- En Low y Mid, una cara con cielo teñido y una lámpara, los dos variando, muestra uno de los dos
  plano (un escalón de 1 o 2 niveles en el borde de la cara). High no.

## Alternativas descartadas

- **A, RGB en todo vóxel.** Exacta, pero paga en todo el mundo: memoria de luz doble, paleta de
  16 B y 64 MiB más de VRAM fija en Low, sin vidrio en ningún lado.
- **B, un índice de tinte por vóxel.** Un vóxel con cielo rojo desde un lado y blanco desde otro no
  se puede decir con un nivel y un tinte; se ven cortes donde cambia el camino ganador.
- **C, tinte solo para el cielo directo.** Las ventanas, el caso principal, no dan nada, y los
  bordes de los techos brillan blancos.
- **D' en todos los tiers.** Caras siempre exactas, pero pide un tercer vertex buffer en Low
  (32 MiB fijos más, o un segundo pipeline). La decisión 1 lo deja solo para High.
