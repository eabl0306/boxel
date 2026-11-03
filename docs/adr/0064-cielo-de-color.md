# ADR 0064: cielo de color a través del vidrio

- Estado: aceptado (issue #11, fase P8 del plan de luz). El dueño aprobó el plan
  (`docs/planes/colored-sky-plan.md`) el 2026-11-02, con sus respuestas a las preguntas abiertas
  (sección 11 del plan). Se construye por carriles (sección 7 del plan). Hechos: el C0 (este ADR),
  el C1 (la luz, en `boxel_light`), el C2 (el mundo, en `boxel_world`) y el C3 (la malla, en
  `boxel_mesh`). Faltan el C4 (render y shaders) y el C5 (depuración, demo y herramientas).
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

### 8. Malla y GPU (carril C3 hecho, sección 11; C4 por construir)

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

### 10. Carril C2: cómo quedó

- Con `colored_sky` prendido, las caminatas, los tiles de rayos y `Reference` usan
  `walk_colored` y `build_colored`. Bajo el rango del generador, un chunk cargado con solo vidrio
  también cuenta como contenido. `SkyMap::open` guarda tintes, para que una edición le agregue
  uno.
- Ediciones: un filtro de opacidad 0 puesto, cambiado o quitado sobre `T` llama a `set_filter` y
  remallea, en el mismo lote, la franja de su columna de 14 bajo `T` a 14 sobre el filtro. Bajo
  `T` alcanza el radio de luz de la edición (el arreglo de 24eeb56). `edit_top` sube también con
  los filtros. Un filtro cambiado en un rayo lo retiñe en el lugar.
- Lo que no estaba en el plan: cuando un tile caminado de nuevo aterriza (una columna pendiente,
  o un chunk editado que se descarga sin persistencia), la franja que se remallea cuenta también
  las columnas cuyos tintes cambiaron, y lo mismo al aterrizar un tile de rayos.
- Nuevos: `ChunkMap::sky_rgb_at`, `SkyMap::colored_sky`, `Reference::mesh_light` y, en
  `WorldStats`, `colored_light_boxes`, `colored_padded` y `colored_chunk_light` (desde el inicio).
- Las luces dinámicas horneadas (ADR 0063) siguen iguales sobre las cajas teñidas.
- Límite: con `SkyRule::Open` (un generador que no declara su techo) no hay tiles, así que el
  vidrio no tiñe el cielo.

### 11. Carril C3: cómo quedó

- **Entrada teñida**, en `[u32; 2]`: palabra 0 = `c0 | c1 << 12 | (plano >> 4) << 24`, palabra 1 =
  `c2 | c3 << 12 | (plano & 15) << 24 | etiqueta << 28`. Cada esquina y el valor plano son rojo,
  verde y azul en el orden de los nibbles de bloque de un `LightValue`. Etiqueta 0: esquinas de
  cielo y bloque plano; 1: al revés. Los bits 29 a 31 quedan en 0 (`layout::TINT_*`).
- **Caras exactas en High sin buffer nuevo en Low**: `ChunkMesh::tinted_exact` trae, por entrada
  teñida, las 4 esquinas del tipo que la entrada deja plano (8 B, mismo formato de esquinas). Solo
  High lo lee. La paleta se deduplica por las esquinas exactas (96 bits), así los índices de los
  quads sirven en los tres tiers; Low y Mid pueden ver dos entradas iguales.
- Mientras se malla, las teñidas toman índices desde 4095 hacia abajo y las normales desde 0. Al
  final, si hay teñidas, una pasada suma `t` a las normales y da vuelta las teñidas. Con `t = 0`
  no se toca nada.
- Una entrada normal de un chunk teñido lleva el cielo de la imagen (6 bajo vidrio negro), no el
  del gameplay. Un `Colored` cuyo cielo es igual al sin teñir en todo vóxel da la malla de antes
  byte a byte (hay prueba).
- **Celdas**: `LitCellQuad` no cambia, para no romper `boxel_render`. Su `light` lleva como cielo
  el canal más brillante, y `ChunkMesh::cell_sky_rgb` (vacío si ninguna celda está teñida) los tres
  canales, en el orden de `cell_quads`. El C4 puede pasarlo a `LitCellQuad` cuando toque el render.
- `corner_light` de una entrada teñida da el canal más brillante; `corner_colors` da lo que dibujan
  Low y Mid, y `exact_corner_colors` lo que dibuja High. El oráculo es
  `reference_colored_face_shading`.
- La escena `roofed` de los benches de luz pasó a `boxel_light/benches/scenes`, para usarla
  también en los de malla.

Para el C4:

- **High**: subir `tinted_exact` al arena justo después de la paleta (`arena_len` suma `t`) y leer
  `light_base + light_len + i` para un quad con índice `i < t`. `t` va en `ChunkGpu.flags` bits 7
  a 19.
- **Mid**: `t` en `ChunkGpu.flags` como High; no necesita `tinted_exact`.
- **Low**: `t` en `ChunkImmediate.origin.w`; la expansión copia las entradas tal cual.
- **Shader**: índice `< t` es teñida. Decodificar con la etiqueta (bit 28 de la palabra 1),
  interpolar las esquinas del tipo que las tiene y usar el plano tal cual (Low y Mid), o
  interpolar también las esquinas de `tinted_exact` (High). Un bit plano por quad que diga
  "teñida" no puede ir en `VsOut::ao` (sección 8).
- **Celdas**: `CellVertex` suma el `u32` de `cell_sky_rgb` (o el cielo gris de `light` si está
  vacío).

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

Carril C3, criterion `gather_mesh`, una corrida por lado contra main (efb7b76):

| Bench | main | rama | Diferencia |
|---|---:|---:|---|
| `gather_mesh/cave_lit` | 118,4 µs | 119,9 µs | +1,3 %, ruido |
| `gather_mesh/surface` | 164,1 µs | 166,2 µs | +1,3 %, ruido |
| `gather_mesh/glass_roof` (la escena de `light/box_glass_roof`, 85 de 86 entradas teñidas) | | 220,8 µs | +27 % sobre su gemelo |
| `gather_mesh/glass_roof_clear` (el mismo techo de vidrio común) | | 174,1 µs | |

La meta del plan para `glass_roof` era +40 % o menos. Sin vidrio, el camino de siempre queda
igual porque `face_corner_lights` se fuerza inline: el camino de color también la llama, y sin
eso el compilador la sacaba del bucle caliente (+3 a +5 % en `mesh/surface`).

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
