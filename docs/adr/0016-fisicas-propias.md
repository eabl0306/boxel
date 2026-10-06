# ADR 0016: física propia mínima

- Estado: aceptado (fases P0, P1 y P2 hechas; P3 en adelante, pendientes)
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

Hubo dos propuestas de diseño (A y B) y una revisión que eligió A como base y le sumó cinco
piezas de B; la comparación está en "Alternativas descartadas".

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

  La fachada tiene la feature `physics` (implica `voxel`), activada por defecto: el crate se
  compila con ella. `PhysicsPlugin` no está en `DefaultPlugins`: cada juego lo agrega.

- **2. Posiciones exactas en cualquier lugar del mundo.** `boxel_voxel::WorldPos { block:
  BlockPos, frac: DVec3 }`: el bloque en `i64` más el desplazamiento dentro de él, con `frac` en
  `[0, 1)`. Toda la física calcula en el marco de un bloque, con números chicos: los bloques son
  cajas enteras exactas y la tolerancia de `1e-7` vale igual en el origen y en `2^61`. La
  diferencia entre dos posiciones resta primero los bloques en `i64` (`delta_to`). `Transform`
  pasa a ser una vista de la posición, con el único redondeo del sistema. Consecuencia que se
  prueba: la misma escena corrida por un vector entero da resultados idénticos bit a bit. Los
  bloques de un `WorldPos` van de `-2^61` a `2^61 + 31` (los de los chunks dentro del límite);
  una posición que se pasa queda sobre el límite, sin volver al último bloque.

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

  Las cajas obstáculo se miran antes que los bloques: el recorrido por capas se corta en el
  obstáculo más cercano, así un chunk sin cargar detrás de él no frena el movimiento. Las
  entradas raras tienen un resultado definido: una forma más fina que `2 * eps` se ensancha a
  `4 * eps` (una esfera diminuta no se cuela entre bloques), el inicio se normaliza y `eps` y el
  empuje máximo se acotan.

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
  una arista o un vértice) avanzan x, después y, después z: de los bloques que el rayo solo toca
  en esa arista o vértice visita los de esa escalera (uno de los dos de una arista). Filtros:
  `Collision` (bloques llenos), `Outline` (bloques con geometría, fluidos opcionales: para
  apuntar) y `Custom`. Un rayo que empieza dentro de un bloque que acepta lo devuelve con
  `t = 0` y sin cara. El punto de impacto queda exactamente sobre el plano de la cara. Todo rayo
  termina: a lo sumo `MAX_RAY_DISTANCE` (4096) bloques por eje, y una dirección con NaN o
  infinito solo mira el bloque de origen (antes, un `max_t` infinito por el aire de arriba
  recorría `2^61` bloques).

- **7. Determinismo.** Los floats solo pasan por `+ - * /`, `sqrt`, `floor`, `ceil`, `abs`,
  `min` y `max`, que IEEE 754 define igual en todas las plataformas. `crates/boxel_physics/
  clippy.toml` prohíbe `mul_add` (FMA), `powi` y todas las funciones de libm (`powf`, `exp`,
  `ln`, `sin`, `atan2`, `hypot` y parecidas) para `f64` y `f32`, y lo mismo más los ayudantes de
  ángulos y rotaciones (`angle_between`, `slerp`, `to_angle`...) para `DVec3`, `Vec3`, `DVec2` y
  `Vec2`. Desde P3: pares y
  eventos en orden de `Entity`, mapas con `foldhash::FixedState` que nunca se recorren para
  ordenar, y paralelismo solo por cuerpo.

- **8. Tiempo y orden.** Los pasos de física corren a 64 Hz en `FixedPostUpdate` (desde P2), así
  el gameplay de `FixedUpdate` no tiene que ordenarse contra ellos. `boxel_app` suma los sets
  `RunFixedMainLoopSystems::{BeforeFixedMainLoop, FixedMainLoop, AfterFixedMainLoop}` (los
  nombres de Bevy) para muestrear la entrada antes de los pasos e interpolar después (con
  `FixedTime::overstep_fraction_f64`, en `f64` como las posiciones), y
  `FixedTime::set_max_steps_per_frame` para bajar el tope de pasos por frame (8 por defecto).
  `PhysicsPlugin` lo baja a 3 en `Plugin::finish` (`PhysicsPlugin::max_steps_per_frame`; `None`
  deja el de la app): después de un tirón, un frame corre a lo sumo 3 pasos. El tope es de toda
  la app, no solo de la física: vale para todo `FixedMain` (también `FixedUpdate`) y el tiempo
  que sobra se descarta (un overrun, avisado a lo sumo cada 5 s). Por eso también es un piso de
  frames: por debajo de la frecuencia fija dividida por el tope (64 / 3 = 21.3 fps; 42.7 fps a
  128 Hz), el juego de paso fijo va en cámara lenta. `finish` pisa un tope puesto antes en el
  `build` de otro plugin; quien quiera otro lo cambia después de `App::finish`. `voxel_world`,
  con un solo cuerpo, pasa `None` y deja los 8 pasos del motor. También pone `FixedPostUpdate` y
  `RunFixedMainLoop` en el ejecutor de un hilo (`PhysicsPlugin::single_threaded`, activado por
  defecto): los sistemas de física van encadenados, así que el ejecutor multi-hilo no gana nada
  y cuesta unos 24 µs por corrida solo en repartir sistemas entre hilos (medido en
  `FixedPostUpdate`: un paso sin cuerpos baja de 25 a 0.65 µs, y uno con 1000 cuerpos de 0.32 a
  0.22 ms). `RunFixedMainLoop` corre cada frame y también es una cadena (antes del bucle, el
  bucle exclusivo, después).

- **8b. Cuerpos (P2).** Componentes:
  - `RigidBody::{Dynamic, Kinematic, Static}` requiere `Transform`, `Velocity`, `BodyPosition`,
    `Contacts`, `BodySettings` y `PhysicsSync`.
  - `Collider { shape, offset, response, layers }`: con `standing`, el origen va en los pies (un
    jugador es `Collider::standing(0.6, 1.8)`); `aabb` y `sphere` van centradas en la posición.
    Contra los vóxeles una esfera choca como su caja. Sin `Collider`, el cuerpo choca como un
    punto. `response` (sólido o sensor) y `layers` se usan desde P3.
  - `Velocity` (bloques por segundo, el gameplay la escribe), `BodyPosition(WorldPos)` (la
    posición que manda), `BodySettings` (escala de gravedad, arrastre horizontal y vertical,
    fricción en el suelo, restitución, velocidad mínima de rebote, peso para P3; presets
    `player`, `item`, `arrow` y `ball` con los valores de Minecraft pasados a segundos),
    `Contacts` (caras recortadas, en el suelo, bloque de apoyo, en fluido), `Frozen`,
    `Interpolated`, `Teleport(WorldPos)`, `CharacterController` y `MoveIntent`.
  - Recursos `PhysicsSettings` (gravedad `(0, -32, 0)`, `eps`, tope de 100 bloques/s por eje,
    empuje máximo de 1 bloque por paso, impacto desde 4 bloques/s) y `PhysicsStats` (la línea
    `physics` de `DiagnosticsBoard`, en `Last`). Mensaje `VoxelImpact { entity, face, block,
    speed }`, ordenado por entidad.

  El paso de un cuerpo dinámico es `step_body`, en el núcleo puro, con `dt` de
  `FixedTime::timestep()` en `f64` (`2^-6` a 64 Hz): gravedad, arrastre implícito
  (`v /= 1 + k * dt`, solo multiplicar y dividir, estable a cualquier frecuencia; `k` es la
  fricción del suelo si el paso anterior terminó apoyado), tope por eje, cero exacto por debajo
  de `STOP_SPEED` (10^-6 bloques/s: el arrastre solo achica la velocidad en proporción, así que
  sin esto un deslizamiento nunca terminaba y `Velocity` cambiaba cada paso durante más de un
  minuto), barrido con `move_box` y, en cada eje recortado, rebote (`v = -v * restitución`) o
  cero si la velocidad no supera la mínima de rebote ni un paso de gravedad en ese eje
  (`|g| * escala * dt`, con lo que un cuerpo apoyado choca el suelo cada paso; sin ese piso, un
  cuerpo con mínima de rebote baja rebotaba en el lugar para siempre). El resto del paso después
  de un rebote se pierde, como en Minecraft. Un recorte más rápido que el umbral es un
  `VoxelImpact` con el bloque que lo frenó (`SweepOut::blocks`). `predict` es el mismo
  integrador sin vóxeles, igual bit a bit a un vuelo libre (para apuntar). Dormir (P4) usa su
  propio umbral, más grande: `|v| < 0.05` bloques/s durante 32 pasos.

  Sets encadenados en `FixedPostUpdate`: `PhysicsSystems::{Prepare, Controller, Kinematic,
  Step, Broadphase, Solve, Events, Writeback}` (`Broadphase` y `Solve` vacíos hasta P3), e
  `Interpolate` en `AfterFixedMainLoop`.
  - Prepare: un cuerpo nuevo toma su posición de `Transform`; un `Teleport` la pone exacta y se
    borra; un `Transform` distinto del que escribió la física (`PhysicsSync`) es un
    teletransporte del gameplay. Un teletransporte conserva la velocidad y borra `Contacts`: el
    cuerpo ya no está sobre su suelo viejo. Los cuerpos interpolados guardan la posición de
    antes del paso. Insertar `Teleport` también pone `Transform` en el destino en el acto (un
    hook), así que un cuerpo creado solo con `Teleport` no se ve en el origen hasta su primer
    paso. Escribir `BodyPosition` directo en un paso fijo también lo mueve, pero no es un
    teletransporte: conserva velocidad y `Contacts`, y si además se escribió `Transform`, gana
    `Transform`.
  - Controller: va después de Prepare, así un cuerpo teletransportado está en el aire para su
    controlador (antes, uno que estaba apoyado podía saltar en el aire en el destino y frenaba
    con la fricción del suelo un paso). Solo mueve cuerpos dinámicos.
  - Kinematic: posición más velocidad por `dt`, a través de todo.
  - Step: si el paso necesita un chunk sin cargar no pasa nada (posición, velocidad, sin
    gravedad), el cuerpo recibe `Frozen` y reintenta el paso siguiente.
  - Writeback: `Transform.translation` (el único redondeo) y `PhysicsSync`. La rotación nunca
    se toca.
  - Interpolate: con `Interpolated`, `Transform = prev + (curr - prev) * alpha` con
    `alpha = overstep_fraction_f64`. `Update` y el render ven la posición interpolada (hasta un
    paso, 15.6 ms, detrás). Un teletransporte mueve las dos puntas: no deja estela.

  Controlador: `CharacterController { walk_speed: 4.317, air_control: 1, jump_speed: 9.5 }`
  convierte `MoveIntent { wish, jump }` en velocidad al principio del paso (set `Controller`):
  acelera `walk_speed * k`, con `k` el arrastre que aplica el paso, así la velocidad tope es
  `walk_speed` con cualquier `BodySettings` de tasas positivas (con `k = 0` no hay velocidad
  tope y el controlador no acelera: sin arrastre en el aire no hay control en el aire, y sin
  fricción no se camina). El salto es un pestillo: el juego lo pone en `true`
  al muestrear la entrada una vez por frame en `BeforeFixedMainLoop` y el paso siguiente lo
  consume (salta si está apoyado). Una pulsación en un frame sin paso salta igual, y un frame con
  dos pasos salta una vez. La IA escribe `MoveIntent` en `FixedUpdate`.

  El plan pedía un salto de 9 bloques/s "para unos 1.25 bloques de alto". Con la gravedad antes
  del movimiento a 64 Hz, 9 bloques/s sube 1.11; 9.5 sube 1.24, el alto de Minecraft, y alcanza
  para subir un escalón de un bloque. Se tomó 9.5.

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

Hecho (P2):

- `boxel_physics`: el integrador (`step_body`, `integrate_velocity`, `predict`, `BodySettings`,
  `StepParams`), los componentes, recursos y el mensaje de 8b, `PhysicsSystems`,
  `CharacterController` con `MoveIntent`, la línea `physics` del tablero y el tope de 3 pasos.
  `SweepOut::blocks` dice qué bloque recortó cada eje.
- `voxel_world`: G cambia entre volar y caminar (un cuerpo de jugador con controlador y la cámara
  en el ojo, a 1.62 bloques); `--walk` arranca caminando sobre el suelo. F3 muestra el modo y la
  línea de física. Espacio pone el pestillo si está apretada o si se apretó en ese frame (una
  pulsación más corta que un frame largo también salta). El giro del mouse sigue en `Update`
  (`FlyCameraSystems`), no antes del bucle fijo como dice el plan: caminar usa el yaw del frame
  anterior, un frame de atraso (7 ms a 144 fps) que no se nota.
- Ejemplo nuevo `physics_sandbox`: arranca caminando sobre el bosque, G vuela, Q tira un objeto
  (caja sensor de un cuarto de bloque con los valores de objeto). `--drop-test` suelta 100
  objetos desde 40 bloques y sale con 0 si todos quedan quietos, o 3 si alguno se mueve después
  de 20 s simulados. Los cuerpos todavía no se dibujan (no hay renderer de entidades): los
  objetos se ven en F3. Romper y poner bloques con la comprobación de lugar libre llega en P3.

Tests de P2:

- `boxel_physics/tests/body.rs`, sin `App`: caer 40 bloques al plano exacto con un solo impacto
  cuya velocidad es la del integrador, 10 000 pasos apoyado sin cambiar un bit, velocidad
  terminal y tope por eje, fricción contra arrastre, una pelota que rebota cada vez más bajo
  hasta parar, una pelota que queda quieta (velocidad 0 exacta) aunque su velocidad mínima de
  rebote sea 0 o la frecuencia fija sea 10 Hz, un deslizamiento que termina en 0 exacto, chocar
  una pared (bloque y cara del impacto), un chunk sin cargar que no mueve nada, `predict` igual
  bit a bit a un vuelo, un salto que sube entre 1.2 y 1.3 bloques, y vuelos idénticos bit a bit
  corridos por 2^31, 2^40, 2^52 y ±(2^61 - 64).
- `boxel_physics/tests/plugin.rs`, una `App` sin ventana con un reloj sintético: caer 40 bloques
  al plano exacto con un solo `VoxelImpact` igual al del integrador puro; congelarse sobre
  chunks sin cargar y caer cuando cargan; un bloque puesto adentro de un cuerpo lo empuja afuera
  y sacar el suelo lo deja caer; teletransporte por `Transform` y por `Teleport`; un
  teletransporte desde el suelo con el salto apretado no salta en el aire y frena con el
  arrastre del aire; un cuerpo creado solo con `Teleport` se ve en su lugar antes del primer
  paso; un despawn aplicado antes del `Frozen` del mismo paso no entra en pánico; un controlador
  en un cuerpo cinemático o estático no lo mueve; la interpolación queda entre los dos pasos y
  no deja estela; a 144 fps llegan a `Update` todos los impactos; el pestillo del salto salta
  una vez con 0 o con 2 pasos en el frame; caminar llega a la velocidad tope y sube un escalón
  saltando; las estadísticas y la línea del tablero. Un test unitario fija el orden de los
  impactos (por entidad y, dentro de cada una, por eje X, Y, Z).
  Determinismo: una escena con caminantes de IA, pelotas, objetos, una pared y un cuerpo
  cinemático da el mismo estado bit a bit tras 1000 pasos en dos corridas, a 64 o a 144 fps y
  con o sin interpolación; la misma escena corrida por 2^31, 2^40, 2^52 y ±(2^61 - 1024) también.
- `tests/ambiguity.rs` ya pone estrictos `FixedPostUpdate` y `RunFixedMainLoop` con
  `PhysicsPlugin`: los sistemas nuevos no tienen pares ambiguos.
- `voxel_world` (sin ventana, `Update` estricto): G, caer al pasto, caminar con W hacia donde
  mira la cámara, saltar con Espacio (también con una pulsación que se suelta en el mismo
  frame), volver a volar.
- `physics_sandbox` (sin ventana): el drop test deja los 100 objetos quietos exactamente sobre el
  pasto, y Q tira un objeto hacia donde mira la cámara.

Hecho antes (P0 y P1):

- `boxel_voxel::WorldPos` con `from_dvec3`, `from_block`, `to_dvec3`, `offset`, `delta_to` y
  `normalize` (con el acarreo cuando un `frac` negativo diminuto redondea a 1, y sujeto al
  límite del mundo).
- `boxel_app::RunFixedMainLoopSystems` (encadenados, con `run_fixed_main_loop` en
  `FixedMainLoop`), `FixedTime::set_max_steps_per_frame` (0 entra en pánico) y
  `FixedTime::overstep_fraction_f64`.
- `boxel_physics`: `CollisionTable`, `VoxelSource`, `TestWorld`, `Shape` y `LocalBox` con rayos
  contra cajas y esferas, `move_box`, `raycast_voxels`, `ChunkSource`, `PhysicsPlugin` (arma la
  tabla en `Plugin::finish` desde `WorldContent`; sin `WorldPlugin` entra en pánico con un
  mensaje claro) y `PhysicsQuery` (`raycast_voxels`, `probe`, `voxels`, `collision`).
- `voxel_world`: con el mouse capturado, clic izquierdo rompe el bloque apuntado y clic derecho
  pone el elegido (teclas 1 a 9) en la cara apuntada, con alcance de 6 bloques. F3 muestra el
  bloque apuntado, su cara y la selección, y hay una mira en el centro. Para eso `boxel_debug`
  suma `OverlayLines` (líneas de las apps en el overlay) y `Crosshair`. La cámara libre suelta
  el cursor al perder el foco, así el clic que vuelve a la ventana no rompe un bloque.

Tests que lo vigilan:

- `boxel_voxel/tests/far_coords.rs`: `WorldPos` exacto hasta el límite, el acarreo, la sujeción
  (sin volver atrás dentro del último bloque), `delta_to` de punta a punta del mundo y una
  proptest de invariancia por traslación.
- `boxel_app/tests/time.rs`: los sets antes y después corren una vez por frame alrededor de 0, 2
  o 3 pasos, y el tope se puede bajar.
- `boxel_physics/tests/sweep.rs`: aterrizaje exacto en el plano, la regla del orden de ejes,
  deslizar por las seis superficies sin engancharse en bloques ni chunks (coordenadas
  negativas), 10 000 pasos apoyado sin cambiar un bit, despenetración (dirección y desempate),
  obstáculos (y chunks sin cargar detrás de ellos), fluidos, chunks sin cargar, el límite del
  mundo, formas finas y entradas raras, y una proptest sobre campos aleatorios (0 a 60 % de
  piedra, movimientos de hasta 50 bloques): nunca termina dentro de un bloque, cada tramo por eje
  no cruza ninguno, cada cara marcada toca de verdad y nada se mueve más de lo pedido.
- `boxel_physics/tests/raycast.rs`: el DDA contra un oráculo de fuerza bruta independiente
  (slabs con división exacta contra cada bloque y la regla de empates escrita aparte): el camino
  completo y el primer impacto, en rayos aleatorios y en rayos diádicos que pasan justo por
  aristas y vértices. Casos a mano: ejes, aristas, vértices, componentes en cero o degeneradas,
  empezar dentro o sobre un borde, `max_t` inclusivo, el tope de distancia, el aire fuera de los
  límites verticales, chunks sin cargar, filtros, almacenamiento uniforme contra empaquetado.
- `boxel_physics/tests/far_coords.rs`: un recorrido de barridos (caer, chocar, nadar, ser
  empujado afuera, congelarse) y rayos, corridos por 0, ±2^31, 2^40, 2^52 y ±(2^61 - 64) en cada
  eje y en los tres: idénticos bit a bit.
- `boxel_physics/tests/query.rs`: `ChunkSource` sobre un `ChunkMap` real en cada estado (listo,
  uniforme, `Missing`, en cola, sin slot, fuera de los límites verticales) y `PhysicsQuery` desde
  un sistema.
- `tests/ambiguity.rs`: ahora con `PhysicsPlugin` (con la feature `physics`) y estricto también
  en `RunFixedMainLoop` y `FixedPostUpdate`.
- `voxel_world`: un test sin ventana rompe y pone bloques con clics simulados, con la cámara
  libre y `Update` estricto: el clic que captura el cursor (también al volver a la ventana) no
  edita.
- Cambiar el barrido para que saltee la última capa, o la cara del raycast, hace fallar las
  proptests (probado a mano).

Costo de P2 (`tick/N` y `frame/1000` en `crates/boxel_physics/benches/physics.rs`, detalle en
[rendimiento/fisicas-p2-2026-10](../rendimiento/fisicas-p2-2026-10.md)): un paso de física
completo (`FixedPostUpdate` con todos los sistemas) sobre el bosque cuesta 0.48 µs sin cuerpos,
24 µs con 100 y 0.23 ms con 1000 cuerpos despiertos e interpolados (objetivos 20 µs, 0.05 ms y
0.5 ms). Interpolar 1000 cuerpos en un frame sin paso (`RunFixedMainLoop`) cuesta 5.1 µs con el
ejecutor de un hilo y 13.6 µs con el multi-hilo.

Costo de P1 (`crates/boxel_physics/benches/physics.rs`, detalle en
[rendimiento/fisicas-p1-2026-10](../rendimiento/fisicas-p1-2026-10.md)): un barrido de una caja
de jugador cuesta entre 0.09 y 0.14 µs (objetivo 0.3 µs), y los rayos `pick_6`, `cave_256` y
`sky_1024` cuestan 0.09, 1.9 y 7.0 µs (objetivos 0.5, 5 y 10 µs). El rayo largo por el cielo
cumple sin saltar chunks de aire enteros, así que ese atajo no se agregó.

Artefactos aceptados, como en Minecraft: un movimiento en diagonal puede cortar una esquina
exterior hasta un paso de movimiento, y el resultado depende del orden de los ejes (fijado por
tests).

Cambios de API de P2:

- `PhysicsPlugin` deja de ser un struct unitario: `PhysicsPlugin::default()`, con los campos
  `max_steps_per_frame` (`Some(3)` por defecto) y `single_threaded` (`true`: `FixedPostUpdate`
  y `RunFixedMainLoop` corren en un hilo).
- `SweepOut` suma `blocks`.
- `boxel_physics` depende también de `boxel_transform` y `boxel_diagnostic`.
- Nuevos: los tipos de 8b, `PhysicsSystems`, `step_body`, `integrate_velocity`, `predict`,
  `StepParams`, `BodyStep`, `BodyImpact` y `STOP_SPEED`; el prelude de la física suma los
  componentes, el mensaje y los sets.
- Sin `TimePlugin` la física no da pasos (no hay `FixedMain`), y la interpolación no hace nada.

Cambios de API de P0 y P1:

- Nuevos: `boxel_voxel::WorldPos`, `boxel_app::RunFixedMainLoopSystems`,
  `FixedTime::set_max_steps_per_frame`, `FixedTime::overstep_fraction_f64`, el crate
  `boxel_physics` (`boxel::physics`, en el prelude), `boxel_debug::{OverlayLines, Crosshair}`.
- `FlyCameraPlugin` suelta el cursor cuando la ventana pierde el foco.
- El ejemplo `voxel_world` pide la feature `physics`.
- El prelude de `boxel_app` suma `FixedPostUpdate`, `RunFixedMainLoop` y
  `RunFixedMainLoopSystems`.
- Un sistema agregado a `RunFixedMainLoop` sin set ahora queda ambiguo con el bucle: va en
  `BeforeFixedMainLoop` o `AfterFixedMainLoop`.

Lo que falta: colisiones entre entidades (P3: grilla, empujes entre sólidos, sensores y sus
eventos, capas, obstáculos estáticos y cinemáticos, raycast de entidades, `overlap_box` y la
comprobación de lugar libre), proyectiles, dormir y paso en paralelo (P4), y todo P5. Los cuerpos
no tienen dibujo: hace falta un renderer de entidades. Los mensajes del mundo que comparten física y luz
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
