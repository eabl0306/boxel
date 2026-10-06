# ADR 0016: física propia mínima

- Estado: aceptado
- Fecha: 2026-10-06

## Contexto

Un juego tipo Minecraft hecho con boxel necesita física: caminar sobre bloques, caer, chocar con
paredes, apuntar a un bloque para romperlo o poner otro, objetos que caen y se juntan,
entidades que se empujan, flechas. Hasta ahora la física estaba en los no objetivos del motor.

Decisiones del dueño (2026-10-06):

- Física propia y mínima: cajas alineadas a los ejes (AABB) y esferas, gravedad, sin
  rotaciones. Rebotes y flechas después. Raycast.
- Entre entidades, cada una elige cómo responde: sólida (empuja, sin solaparse) o sensor (solo
  avisa cuándo empieza y termina el contacto).
- Sin motores de física de terceros.

Hubo dos propuestas (A y B, en `.claude/plans/light-physics-proposals.json`) y una revisión que
eligió A como base y le sumó cinco piezas de B (`.claude/plans/physics-plan.md`).

Dos datos del código pesan en el diseño:

- `Transform` guarda la traslación en `f64` sin origen flotante (ADR 0014). Un contacto necesita
  una tolerancia de unos `1e-7` bloques, y el paso de un `f64` pasa de `1e-7` cerca de `2^29`.
  Con posiciones absolutas en `f64`, más allá de ahí los cuerpos se hundirían en el suelo o se
  engancharían.
- El mundo vive en `ChunkMap` (chunks de 32^3 con paleta). Leer un bloque cuesta unos 0.9 ns
  dentro de un chunk ya encontrado y 6.5 ns buscar el chunk.

## Decisión

- **1. Un crate propio, `boxel_physics`.** Depende de `boxel_ecs`, `boxel_app`, `boxel_math`,
  `boxel_voxel` y `boxel_world`; ni `boxel_world` ni `boxel_render` dependen de él. Tiene dos
  capas:
  - `core`, puro (sin ECS, solo `glam` y `boxel_voxel`), que se prueba y se mide sin `App`:
    tabla de colisión, fuentes de vóxeles, formas, barrido y raycast.
  - La capa ECS: `ChunkSource`, `PhysicsPlugin` y el `SystemParam` `PhysicsQuery`.

  La fachada tiene la feature `physics` (implica `voxel`), activada por defecto. El crate se
  compila siempre, pero `PhysicsPlugin` no está en `DefaultPlugins`: cada juego lo agrega.

- **2. Posiciones exactas en cualquier lugar del mundo.** `boxel_voxel::WorldPos { block:
  BlockPos, frac: DVec3 }`: el bloque en `i64` más el desplazamiento dentro de él, con `frac` en
  `[0, 1)`. Toda la física calcula en el marco de un bloque, con números chicos: los bloques son
  cajas enteras exactas y la tolerancia de `1e-7` vale igual en el origen y en `2^61`. La
  diferencia entre dos posiciones resta primero los bloques en `i64` (`delta_to`). `Transform`
  pasa a ser una vista de la posición, con el único redondeo del sistema. Consecuencia que se
  prueba: la misma escena corrida por un vector entero da resultados idénticos bit a bit.

- **3. Colisión con vóxeles: el recorte por ejes de Minecraft.** `move_box` mueve una caja por un
  desplazamiento, un eje por vez: primero Y, después el mayor de X y Z (X si empatan). En cada
  eje recorre las capas de bloques que alcanza la cara delantera, de la más cercana a la más
  lejana, sobre la huella de la caja en los otros dos ejes. La primera capa con un bloque lleno
  (o una caja obstáculo) corta el movimiento y la cara queda exactamente sobre el plano
  (`posición = plano - offset de la cara`), así un cuerpo apoyado calcula siempre la misma
  posición y no deriva. La huella es estricta: un bloque que solo toca la caja no entra, así que
  deslizar por un piso o una pared no se engancha en las costuras entre bloques ni entre chunks.
  El costo crece lineal con la distancia y no hacen falta subpasos.

  Si la caja empieza dentro de bloques llenos (un bloque puesto encima de un cuerpo), primero se
  la empuja hacia el lado libre más cercano a un bloque o menos (desempate +Y, -Y, +X, -X, +Z,
  -Z); si no hay ninguno, un bloque hacia arriba.

  Las esferas chocan con los vóxeles como su caja envolvente en v1. Los rayos contra esferas sí
  son exactos.

- **4. Cómo choca cada bloque.** `CollisionTable` se deriva del registro: `BlockDef::solid` da
  un cubo lleno, la marca `FLUID` del registro da "fluido" y una forma de render distinta de
  `None` da "tiene contorno" (para apuntar). No se agrega un campo `collision` a `BlockDef`
  todavía: el trabajo de luz de colores cambia `BlockDef` en paralelo, y el campo nuevo (para
  losas y escaleras) entra en el mismo cambio, así el struct se rompe una sola vez.
  `CollisionShape` es `non_exhaustive` (`Empty`, `Full`; las cajas vienen después).

- **5. Chunks sin cargar: congelar.** Una fuente de vóxeles responde el bloque o
  `Probe::Unloaded`. Si un barrido necesita un bloque sin cargar, no mueve nada y devuelve
  `Unloaded` (los cuerpos de P2 se congelan y reintentan). Un rayo devuelve
  `RayResult::Unloaded { block, t }`, distinto de no encontrar nada. Un chunk cuya generación
  falló (`Missing`) y las capas fuera de `WorldSettings::vertical_bounds` cuentan como aire.
  `ChunkSource` guarda el último chunk que buscó: una búsqueda por chunk tocado y los chunks
  uniformes sin decodificar.

- **6. Raycast exacto.** DDA de Amanatides-Woo desde un `WorldPos`, con pasos de bloque en
  `i64`. Cada eje guarda la distancia entera de su próximo plano al bloque de origen, y cada
  paso calcula `t = (plano - frac) * (1 / dir)` de cero en vez de acumular: el resultado no
  depende de la distancia recorrida ni de dónde empieza el rayo. Los empates (un rayo justo por
  una arista o un vértice) avanzan x, después y, después z. Filtros: `Collision` (bloques
  llenos), `Outline` (bloques con geometría, fluidos opcionales: para apuntar) y `Custom`. Un
  rayo que empieza dentro de un bloque que acepta lo devuelve con `t = 0` y sin cara. El punto
  de impacto queda exactamente sobre el plano de la cara.

- **7. Determinismo.** Los floats solo pasan por `+ - * /`, `sqrt`, `floor`, `ceil`, `abs`,
  `min` y `max`, que IEEE 754 define igual en todas las plataformas. `crates/boxel_physics/
  clippy.toml` prohíbe `mul_add` (FMA) y las funciones de libm (`powf`, `powi`, `exp`, `ln`,
  `sin`, `cos` y parecidas) para `f64`, `f32` y los vectores de `glam`. Desde P3: pares y
  eventos en orden de `Entity`, mapas con `foldhash::FixedState` que nunca se recorren para
  ordenar, y paralelismo solo por cuerpo.

- **8. Tiempo y orden.** Los pasos de física corren a 64 Hz en `FixedPostUpdate` (desde P2), así
  el gameplay de `FixedUpdate` no tiene que ordenarse contra ellos. `boxel_app` suma los sets
  `RunFixedMainLoopSystems::{BeforeFixedMainLoop, FixedMainLoop, AfterFixedMainLoop}` (los
  nombres de Bevy) para muestrear la entrada antes de los pasos e interpolar después, y
  `FixedTime::set_max_steps_per_frame` para bajar el tope de pasos por frame (8 por defecto; con
  muchos cuerpos se recomienda 3).

- **9. Entre entidades (P3).** Grilla uniforme ordenada de celdas de 4 bloques. Los sólidos se
  separan por posición en todos los ejes, con apilado, y cada empuje pasa por el barrido de
  vóxeles (las paredes siempre ganan). Los sensores emiten `CollisionStarted` y
  `CollisionEnded`. Los sólidos estáticos y cinemáticos son obstáculos dentro del barrido:
  `move_box` ya recibe esas cajas.

- **10. Fases.** P0: requisitos (`WorldPos`, sets del bucle fijo, el tope de pasos, este ADR, la
  feature). P1: núcleo y raycast (tabla, fuentes, barrido, DDA, `PhysicsQuery::raycast_voxels`,
  edición de bloques en `voxel_world`). P2: cuerpos (gravedad, arrastre, rebote, congelar,
  interpolación, teletransporte, controlador). P3: entre entidades. P4: proyectiles, dormir y
  paso en paralelo. P5, a pedido: subir escalones, formas de bloque con cajas, fluidos, fricción
  por bloque, esfera exacta contra vóxeles.

## Consecuencias

Hecho (P0 y P1):

- `boxel_voxel::WorldPos` con `from_dvec3`, `from_block`, `to_dvec3`, `offset`, `delta_to` y
  `normalize` (con el acarreo cuando un `frac` negativo diminuto redondea a 1, y sujeto al
  límite del mundo).
- `boxel_app::RunFixedMainLoopSystems` (encadenados, con `run_fixed_main_loop` en
  `FixedMainLoop`) y `FixedTime::set_max_steps_per_frame` (0 entra en pánico).
- `boxel_physics`: `CollisionTable`, `VoxelSource`, `TestWorld`, `Shape` y `LocalBox` con rayos
  contra cajas y esferas, `move_box`, `raycast_voxels`, `ChunkSource`, `PhysicsPlugin` (arma la
  tabla en `Plugin::finish` desde `WorldContent`; sin `WorldPlugin` entra en pánico con un
  mensaje claro) y `PhysicsQuery` (`raycast_voxels`, `probe`, `voxels`, `collision`).
- `voxel_world`: con el mouse capturado, clic izquierdo rompe el bloque apuntado y clic derecho
  pone el elegido (teclas 1 a 9) en la cara apuntada, con alcance de 6 bloques. F3 muestra el
  bloque apuntado, su cara y la selección, y hay una mira en el centro. Para eso `boxel_debug`
  suma `OverlayLines` (líneas de las apps en el overlay) y `Crosshair`.

Tests que lo vigilan:

- `boxel_voxel/tests/far_coords.rs`: `WorldPos` exacto hasta el límite, el acarreo, la sujeción,
  `delta_to` de punta a punta del mundo y una proptest de invariancia por traslación.
- `boxel_app/tests/time.rs`: los sets antes y después corren una vez por frame alrededor de 0, 2
  o 3 pasos, y el tope se puede bajar.
- `boxel_physics/tests/sweep.rs`: aterrizaje exacto en el plano, la regla del orden de ejes,
  deslizar por las seis superficies sin engancharse en bloques ni chunks (coordenadas
  negativas), 10 000 pasos apoyado sin cambiar un bit, despenetración (dirección y desempate),
  obstáculos, fluidos, chunks sin cargar, y una proptest sobre campos aleatorios (0 a 60 % de
  piedra, movimientos de hasta 50 bloques): nunca termina dentro de un bloque, cada tramo por eje
  no cruza ninguno, cada cara marcada toca de verdad y nada se mueve más de lo pedido.
- `boxel_physics/tests/raycast.rs`: el DDA contra un oráculo de fuerza bruta (prueba de slabs
  contra cada bloque) en rayos aleatorios, y casos a mano (ejes, aristas, vértices, componentes
  en cero, empezar dentro o sobre un borde, `max_t` inclusivo, chunks sin cargar, filtros,
  almacenamiento uniforme contra empaquetado).
- `boxel_physics/tests/far_coords.rs`: un recorrido de barridos (caer, chocar, nadar, ser
  empujado afuera, congelarse) y rayos, corridos por 0, ±2^31, 2^40, 2^52 y ±(2^61 - 64) en cada
  eje y en los tres: idénticos bit a bit.
- `boxel_physics/tests/query.rs`: `ChunkSource` sobre un `ChunkMap` real en cada estado (listo,
  uniforme, `Missing`, en cola, sin slot, fuera de los límites verticales) y `PhysicsQuery` desde
  un sistema.
- `tests/ambiguity.rs`: ahora con `PhysicsPlugin` y estricto también en `RunFixedMainLoop` y
  `FixedPostUpdate`.
- `voxel_world`: un test sin ventana rompe y pone bloques con clics simulados.
- Cambiar el barrido para que saltee la última capa, o la cara del raycast, hace fallar las
  proptests (probado a mano).

Costo (`crates/boxel_physics/benches/physics.rs`, detalle en
[rendimiento/fisicas-p1-2026-10](../rendimiento/fisicas-p1-2026-10.md)): un barrido de una caja
de jugador cuesta entre 0.09 y 0.14 µs (objetivo 0.3 µs), y los rayos `pick_6`, `cave_256` y
`sky_1024` cuestan 0.09, 1.9 y 7.0 µs (objetivos 0.5, 5 y 10 µs). El rayo largo por el cielo
cumple sin saltar chunks de aire enteros, así que ese atajo no se agregó.

Artefactos aceptados, como en Minecraft: un movimiento en diagonal puede cortar una esquina
exterior hasta un paso de movimiento, y el resultado depende del orden de los ejes (fijado por
tests).

Cambios de API:

- Nuevos: `boxel_voxel::WorldPos`, `boxel_app::RunFixedMainLoopSystems`,
  `FixedTime::set_max_steps_per_frame`, el crate `boxel_physics` (`boxel::physics`, en el
  prelude), `boxel_debug::{OverlayLines, Crosshair}`.
- El prelude de `boxel_app` suma `FixedPostUpdate`, `RunFixedMainLoop` y
  `RunFixedMainLoopSystems`.
- Un sistema agregado a `RunFixedMainLoop` sin set ahora queda ambiguo con el bucle: va en
  `BeforeFixedMainLoop` o `AfterFixedMainLoop`.

Lo que falta: cuerpos, gravedad y colisiones entre entidades (P2 y P3), proyectiles, dormir y
paso en paralelo (P4), y todo P5. Los mensajes del mundo que comparten física y luz
(`BlockChanged`, carga y descarga de chunks) se acuerdan antes de P4.

## Alternativas descartadas

- Un motor de terceros (Rapier, Avian, parry): no conoce la grilla de vóxeles (habría que pasarle
  los bloques como colisionadores), trabaja en `f32` o con posiciones absolutas, trae rotaciones
  y dependencias que no queremos, y su determinismo entre plataformas no está garantizado.
- La propuesta B como base (move-and-slide continuo por tiempo de impacto, esfera exacta contra
  vóxeles, CCD contra entidades desde el principio): más cara (1.3 veces por cuerpo según su
  propia cuenta) y con más lugares donde esconder errores (empates de normales, la piel). Se
  tomaron de B: la interpolación justo después del bucle fijo, el setter del tope de pasos, el
  ajuste exacto al plano, el empuje por posición con apilado y `WorldPos` en `boxel_voxel`.
- Solo `Transform` en `f64` absoluto: los contactos se degradan desde unos `2^29` bloques.
- Tratar lo no cargado como sólido (pisos y techos invisibles, saltos cuando llega el terreno
  real) o como aire (los cuerpos caen al vacío).
- Agregar `BlockDef::collision` ahora: rompería `BlockDef` dos veces junto con el cambio de luz.
