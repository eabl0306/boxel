# ADR 0027: muros de piedra y puertas de valla

- Estado: aceptado
- Fecha: 2026-10-10
- Revisa: ADR 0020, sección 5 (las reglas de vecinos ya no se quedan en una vuelta; las vallas
  se conectan a las puertas), ADR 0021, sección 3 (una celda sin colisión no siempre puede tener
  agua: la puerta abierta no) y ADR 0026 (fuera de esa parte quedaban los muros y las puertas)

## Contexto

El clon de Minecraft necesita muros de piedra y puertas de valla. Son modelos de cajas (ADR 0020)
cuyo estado sigue a los vecinos, y chocan a 1,5 bloques como la valla (ADR 0026). Tienen tres
cosas nuevas:

- **El muro mira arriba.** Su lado es alto si el bloque de arriba lo tapa, y su poste depende del
  poste del muro de arriba. Una regla lee lo que otra cambia: romper la antorcha de arriba de una
  columna de muros cambia los postes de toda la columna, de a un bloque. La vuelta única del ADR
  0020 no alcanza.
- **La puerta se usa.** Un clic derecho la abre o la cierra en vez de poner un bloque. El motor
  no tenía una forma de decir qué hace usar un bloque.
- **Una puerta abierta no choca**, pero se apunta igual y, como en Minecraft, frena el agua y la
  lava.

## Decisión

### 1. Modelos y formas

- `BlockModel` suma `Wall { post, sides }` y `FenceGate { facing, open, in_wall }`. `WallSides`
  guarda un `WallSide` (`none`, `low`, `tall`) por dirección, dos bits cada uno.
- **Muro** (`boxel_voxel::shapes`): las partes de Minecraft (`WallBlock::makeShapes` y los modelos
  `template_wall_*`). El poste es `[4,0,4]-[12,16,12]`; hacia cada lado conectado va una barra de
  6 de ancho (`5..11`), de 14 de alto si es baja y de 16 si es alta. Para dibujar, la barra llega
  al centro (8); para apuntar y chocar, a 11 (pasa el centro, como Minecraft).
- En Minecraft las partes se solapan. Acá la unión se arma en una grilla con cortes en
  `0, 4, 5, 8, 11, 12, 16` y se corta en cajas que no se solapan: primero el poste entero,
  después el resto fila por fila, cada tramo de igual altura estirado mientras las filas de abajo
  lo repiten. Así una barra para en el poste y no se dibujan caras dentro de él, un muro recto
  sin poste es una sola caja y un cruce no tiene caras coplanares que peleen en la profundidad.
  La forma es la misma que la de Minecraft, sexto a sexto.
- Colisión: la misma unión con todo a 24 dieciseisavos (1,5 bloques). Contorno: el de Minecraft,
  poste de 16 y lados de 14 o 16.
- **Puerta**: los modelos `template_fence_gate` y `template_fence_gate_open`, escritos para el sur
  y girados como Minecraft gira sus modelos. Dos postes de 2x11x2 en los extremos; cerrada, dos
  barras de 2x3 a cada lado y una pieza del medio de 4 de ancho (Minecraft usa dos de 2, que con
  la textura por posición muestran los mismos texeles); abierta, las dos hojas giradas hacia
  `facing`. Con `in_wall` todo baja 3 dieciseisavos (`template_fence_gate_wall`) y muestra los
  mismos texeles que la puerta suelta, porque Minecraft no cambia las UV de esos modelos. Por eso
  sus cajas llevan un desplazamiento de textura de 3, como la antorcha de pared.
- Contorno de la puerta: `[0,0,6]-[16,16,10]` girada, de 13 de alto entre muros, igual abierta o
  cerrada (Minecraft). Colisión: la misma caja a 24 de alto cerrada, también entre muros; abierta,
  ninguna.

### 2. Contenido

| Bloque | Estados | Notas |
| --- | --- | --- |
| `boxel:cobblestone_wall` | 257 a 580 (por defecto 263) | `east`, `north`, `south`, `west` (`none`, `low`, `tall`), `up` (por defecto `true`), `waterlogged` |
| `boxel:oak_fence_gate` | 581 a 596 (por defecto 581) | `facing`, `in_wall`, `open` |

- Van al final, después de la obsidiana: los ids anteriores no cambian.
- El muro usa la capa del adoquín y la puerta la de los tablones. No hay capas nuevas: el
  resource pack las llena como antes (Minecraft usa `block/cobblestone` y `block/oak_planks` en
  esos modelos) y el fondo procedural es el mismo.
- Sin `boxel:stone_brick_wall`: no hay textura de ladrillos de piedra.
- Sin `powered`: no hay redstone. La puerta no guarda agua, como en Minecraft. El muro sí
  (`waterlogged`, ADR 0025).

### 3. Reglas de vecinos (las de Minecraft)

- **Conexión del muro** (`wall_connects`): otro muro, el extremo de una puerta (sus dos lados a lo
  ancho de `facing`) o una cara firme, salvo los bloques cutout (las hojas). No una valla: sus
  caras no son firmes, igual que en Minecraft.
- **Lado alto**: el lado conectado es alto si la cara de abajo de la colisión del bloque de
  arriba cubre su franja de prueba (2 de ancho, del centro al borde de ese lado). Un bloque lleno,
  un muro o una valla arriba lo hacen alto.
- **Poste** (`WallBlock::shouldRaisePost`):
  - Debajo de un muro con poste, siempre.
  - Si no, en un extremo, una esquina, una T o un muro solo.
  - Si no (un tramo recto o un cruce): no hay poste si un par opuesto es alto de los dos lados.
    Si no, hay poste con una antorcha de pie arriba (la etiqueta `wall_post_override` de
    Minecraft) o si el bloque de arriba cubre el centro.
- **Puerta**: `in_wall` si hay un muro en uno de sus extremos.
- **Vallas**: ahora también se conectan al extremo de una puerta.
- **Las reglas se propagan.** `NeighborRules::expand` sigue mirando las ediciones y sus 6
  vecinos. Además, cuando una regla cambia un bloque, vuelve a mirar en el mismo lote sus vecinos
  con regla que ya miró (los que todavía no miró van a ver el cambio igual). Como mucho
  `MAX_RULE_UPDATES` (65 536) de esas miradas por lote, más `RULE_UPDATES_PER_CELL` (16) por
  celda del primer anillo, por si alguna regla futura nunca se asienta. Si llega al tope, deja de
  mirar y avisa en el log.
  - El tope crece con el lote porque un lote grande de muros puestos de abajo arriba mira cada
    muro unas 3 veces: cada muro se mira primero cuando el de arriba todavía no tiene su forma.
    Un `--fill` de 64x17x64 muros necesita 250 000 miradas. Con un tope fijo, quedaban muros sin
    asentar.
  - Termina: los muros solo leen hacia arriba, así que el cambio baja por la columna y para. Las
    demás reglas no leen nada que una regla cambie, así que mirarlas de nuevo no cambia nada.
  - Sigue siendo un solo lote: un remallado y un `BlockChanged` por bloque.
  - Un vecino en un chunk que no se puede leer deja el bloque como está, como antes.

### 4. Usar un bloque

- `boxel_voxel::rules::UseRule`: `(registro, estado, hacia dónde mira el jugador) -> Option<estado>`.
  `None` quiere decir que el bloque no se usa y el clic pone un bloque como siempre.
- `boxel_world::BlockUses` (recurso, lo pone `WorldPlugin`): una regla por bloque, como
  `NeighborRules`. `use_block` la aplica. El juego encola el estado nuevo como una edición, y las
  reglas de vecinos corren con ella.
- **Puerta** (`fence_gate_use`, `FenceGateBlock` de Minecraft): abierta, se cierra. Cerrada, se
  abre hacia donde mira el jugador; si la puerta lo miraba, primero se da vuelta. Como gira sobre
  el mismo eje, sus conexiones no cambian.
- `voxel_world`: el clic derecho sobre un bloque que se usa lo usa. Con Shift pone el bloque
  elegido, como en Minecraft. La puerta se pone mirando hacia donde mira el jugador.

### 5. Luz, fluidos, física, malla y GPU

- **Luz**: opacidad 0, como la valla. Un muro con agua tiene 1 (frena el cielo directo).
- **Cuevas**: no ocultan (no son cubos opacos). La ocultación de cuevas los cuenta abiertos.
- **Fluidos**: la puerta, abierta o cerrada, frena el agua y la lava y cuenta como sólida debajo
  de ellas (agua infinita). En Minecraft la puerta es de madera, que `blocksMotion` aunque no
  choque. Hasta ahora los fluidos miraban la colisión, y el agua borraba la puerta abierta. Ahora
  el registro tiene una marca aparte, `StateFlags::BLOCKS_MOTION` (choca, o es una puerta), y
  los fluidos leen esa.
- **Física**: nada nuevo en el código. Son bloques altos (`TALL`, `reach` sigue en 0,5).
  `plane_gap` baja de 0,375 a 0,25 porque hay caras a 4/16 del plano.
- **Malla**: nada nuevo. Van por el camino de cajas del ADR 0020: ocultan sus caras contra los
  vecinos que las cubren (un muro contra el siguiente, el poste de una puerta contra el muro).
- **GPU**: nada nuevo. La tabla de cajas crece pero sigue muy lejos de las 1024.

### 6. `snapshot`

`--place` y `--fill` pasan por las reglas, así que basta con poner los bloques. Por ejemplo, en
el mundo plano, un muro con una puerta metida, dos hileras con una antorcha y una piedra encima:
`--world flat --fill 'cobblestone_wall@0,64,3:8,64,3' --place 'oak_fence_gate[facing=south]@4,64,3'
--fill 'cobblestone_wall@0,65,3:2,65,3' --place 'torch@1,66,3' --place 'stone@6,65,3'
--camera 4.5,66.2,6.6,0,-26 --shot 12:00=muro.png`.

## Consecuencias

- Los chunks sin estos bloques cuestan lo mismo: ni el mesher ni la física cambiaron.
- Las reglas de vecinos ahora usan el hasher rápido de los mapas del mundo (`foldhash`) en vez
  del de la biblioteca estándar, y el anillo guarda el lugar de cada celda para no volver a mirar
  las que todavía no miró. Las segundas miradas no cuestan nada cuando ninguna regla cambia algo.
- Medido con criterion, una corrida por lado, en el Ryzen 7 250 con la RTX 5050 (µs; main es
  ecd7c8f). `expand/*` es el bench nuevo `boxel_world/benches/neighbor_rules.rs` (un lote de 16x16
  sobre un mundo vacío); `mesh/walls` es la escena nueva de `boxel_mesh` (muros cada 6 bloques en
  la superficie, con puertas; 4149 quads). La escena `mesh/shapes` deja afuera muros y puertas para
  seguir siendo la de antes (3812 quads, 2362 caras de caja en los dos lados):

  | Bench | main | rama |
  |---|---|---|
  | `expand/stone` (128 bloques sin regla) | 43,4 | 7,5 |
  | `expand/fences` (256 vallas que se conectan) | 113,2 | 63,0 |
  | `expand/walls` (640 muros en dos hileras y 128 antorchas) | | 234,3 |
  | `mesh/terrain` | 28,5 | 28,4 |
  | `mesh/shapes` | 68,9 | 69,5 |
  | `gather_mesh/shapes` | 145,7 | 148,4 |
  | `gather_mesh/surface` | 166,2 | 167,2 |
  | `mesh/walls` | | 67,7 |
  | `gather_mesh/walls` | | 143,8 |
  | `sweep/player_walk` (ns) | 252,8 | 261,4 |
  | `sweep/step_wall` (ns) | 498,9 | 504,8 |
  | `raycast/sky_1024` | 7,71 | 7,41 |
  | `tick/1000` | 489,2 | 482,8 |

  - La versión de las reglas con el hasher de la biblioteca estándar costaba más que main:
    `expand/stone` 54,2 y `expand/fences` 206,0 (+26 % y +87 %), por las segundas miradas y por
    sacar cada celda del conjunto de pendientes. Por eso el cambio de hasher y el índice del
    anillo.
  - Las mallas y la física no cambiaron de código: sus diferencias son ruido. El mismo binario de
    main, corrido dos veces seguidas, dio `sweep/player_walk` 252,8 y 275,1 y `tick/1000` 489,2 y
    478,3.
  - `mesh/walls` cuesta como `mesh/shapes` por quad: 16,3 ns contra 18,2 ns.
- No se corrió `perf-smoke`: ningún caso de `stress` pone muros ni puertas (el generador no los
  pone), y el mesher, la luz, la física y el render no cambiaron de código.
- En el aire, un muro con poste y cuatro lados son 26 quads (6 del poste y 5 por barra). Un
  tramo recto dentro de un muro largo son 4 (con las cajas de Minecraft serían 8). Una puerta
  cerrada sola son 34.
- Cambios de API:
  - `BlockModel::Wall`, `BlockModel::FenceGate`, `WallSide`, `WallSides`.
  - En `shapes`: `wall_boxes`, `wall_outline`, `wall_collision`, `fence_gate_boxes`,
    `fence_gate_outline`, `fence_gate_collision` y `GATE_IN_WALL_DROP`.
  - En `rules`: `wall_rule`, `wall_shape_of`, `wall_connects`, `gate_connects`, `is_wall`,
    `fence_gate_rule`, `UseRule`, `use_rule_for_model` y `fence_gate_use`.
  - En `boxel_world`: `BlockUses`, `MAX_RULE_UPDATES` y `RULE_UPDATES_PER_CELL`.
  - En `boxel_voxel`: `StateFlags::BLOCKS_MOTION` y `BlockRegistry::blocks_motion`.
  - Un `match` exhaustivo sobre `BlockModel` fuera del motor deja de compilar.
- Tests:
  - `boxel_voxel/tests/walls.rs`: estados e ids. Las cajas, el contorno y la colisión de todos los
    muros (162 combinaciones) son la unión de las partes de Minecraft, sin solapes; las de la
    puerta, sus modelos girados. Entre muros, la puerta muestra los texeles de la puerta suelta. La regla del muro contra una versión de fuerza bruta escrita del
    código de Minecraft, con 10 000 vecindarios al azar, secos y con agua. Los casos a mano
    (extremo, tramo, esquina, cruce, bloque, antorcha y muro arriba). `in_wall` contra fuerza
    bruta. Vallas y muros se conectan a los extremos de la puerta. Usar la puerta la abre hacia
    afuera y la cierra. Colisión a 1,5. Una antorcha se sostiene sobre un muro. La ocultación de
    cuevas los ve abiertos.
  - `boxel_world`: una columna de muros pierde todos sus postes al romper la antorcha, en un lote.
    En cajas al azar de muros, puertas, vallas, piedra y antorchas, después de cada lote toda
    regla queda asentada (sin la propagación, este test falla). Un lote de 69 632 muros queda
    asentado, y una regla que nunca se asienta para en el tope. En el mundo de verdad, un muro con
    una puerta y una valla se conectan y la antorcha baja los postes.
  - `boxel_world/tests/fluid.rs`: el agua y la lava rodean la puerta, abierta o cerrada, y no
    la borran. Dos fuentes sobre una puerta abierta hacen una tercera. El simulador tiene
    puertas, también en el terreno al azar y lejos del origen.
  - `boxel_mesh/tests/shapes.rs`: muros y puertas en los chunks al azar del oráculo, y un tramo
    con una puerta oculta lo que cubre.
  - `boxel_light/tests/shapes.rs`: pasan el cielo; con agua, lo frenan.
  - `boxel_physics/tests/walls.rs`: frenan a un caminante y a un salto, se aterriza a 1,5, la
    puerta abierta deja pasar, los rayos de colisión pegan arriba y los de apuntar no. Igual bit a
    bit en cinco bases lejanas. Los oráculos de `block_shapes.rs` tienen muros y puertas.
  - `boxel_render/tests/gpu_tiers.rs`: la escena especial suma un muro con puerta, esquina, poste
    elevado, lados altos, una puerta abierta y un muro con agua. Los tres tiers dan los mismos
    píxeles.
  - `voxel_world`: el clic derecho abre y cierra la puerta, y con Shift pone un bloque encima.

## Fuera de esta parte

- Muros de otras piedras (ladrillos de piedra, piedra lisa), cuando haya texturas.
- `powered` y la redstone.
- Paneles de vidrio y barrotes, que también se conectan a los muros en Minecraft.
- Animación y sonido al abrir la puerta.

## Alternativas descartadas

- Las cajas de Minecraft tal cual: se dibujan caras dentro del poste y, en un cruce, caras
  coplanares que pelean en la profundidad.
- Una vuelta y leer dos bloques arriba: el poste depende de toda la columna de arriba, no de un
  bloque.
- Que el muro no mire el poste de arriba: los muros apilados no tendrían el poste de Minecraft.
- Abrir la puerta desde una regla de vecinos: abrir es una acción del jugador, no algo que se
  deduzca de los vecinos.
