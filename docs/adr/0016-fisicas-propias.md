# ADR 0016: física propia mínima

- Estado: aceptado (fases P0 a P4 hechas; de P4 falta medir con stress en ventana; P5 a pedido,
  con subir escalones y las formas de bloque hechas)
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
  `None` da "tiene contorno" (para apuntar). `BlockDef` no tiene un campo `collision`
  todavía. El plan era sumarlo junto con la luz de colores, para romper el struct una sola vez,
  pero la luz ya entró (`emission` y `light_opacity`, ADR 0015) sin él. El campo llega con P5
  (losas y escaleras) y rompe `BlockDef` por su cuenta.
  `CollisionShape` es `non_exhaustive` (`Empty`, `Full`; las cajas vienen después).
  **Nota (2026-10-07, ADR 0018):** llegó `BlockDef::collision` (`None`, `Full` o cajas en
  dieciseisavos) y `CollisionShape::Boxes`. La tabla guarda además un contorno para apuntar que
  sale del modelo. El barrido frena en la cara más cercana de las cajas, el raycast pega en la
  forma real y `PhysicsQuery::state_is_free` chequea un bloque a poner con su forma.

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
  `Vec2`. Desde P3: los pares salen ordenados por `Entity` (en P3 porque los cuerpos entraban a
  la grilla en ese orden; desde P4 se ordenan los pares y la lista queda en el orden de la
  query), el solve los recorre en un orden fijo (de abajo hacia arriba, desempate por par) y
  los eventos salen ordenados. La grilla usa un hash de celda fijo y no hay mapas que se
  recorran. El paralelismo es por cuerpo (cada uno lee el mundo y escribe solo lo suyo), sobre
  listas que después se ordenan o que cada hilo llena en un lugar fijo (el orden de la query), o
  por islas del solve (9d), así que el resultado no depende de los hilos. Un test lo compara con
  1, 3 y 8 hilos.

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
  Step, Broadphase, Solve, Events, Writeback}` (`Broadphase` y `Solve` se llenan en P3, ver 9),
  e `Interpolate` en `AfterFixedMainLoop`.
  - Prepare: un cuerpo nuevo toma su posición de `Transform`; un `Teleport` la pone exacta y se
    borra; un `Transform` distinto del que escribió la física (`PhysicsSync`) es un
    teletransporte del gameplay. Un teletransporte conserva la velocidad y borra `Contacts`: el
    cuerpo ya no está sobre su suelo viejo. Los cuerpos interpolados guardan la posición de
    antes del paso. Insertar `Teleport` también pone `Transform` en el destino en el acto (un
    hook), así que un cuerpo creado solo con `Teleport` no se ve en el origen hasta su primer
    paso. Escribir `BodyPosition` directo en un paso fijo también lo mueve, pero no es un
    teletransporte: conserva velocidad y `Contacts`, y si además se escribió `Transform`, gana
    `Transform`. Desde la revisión de P4, `Prepare` reconoce ese empujón a mano por el tick de
    cambio de `BodyPosition`: la física lo escribe siempre antes del `Writeback`, así que un
    cambio más nuevo que el último `Writeback` vino del juego. Lo trata como un teletransporte
    para todo lo demás: despierta al cuerpo dormido, despierta a los que dormían sobre el lugar
    que dejó (lo saca del índice: el almacén de dormidos o la lista del paso) y un estático o
    cinemático movido así queda activo ese paso y empuja a los que encuentra. Guardar la última
    posición en `PhysicsSync` costaba de 3 a 6 % del paso en un hilo; el tick no cuesta nada.
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

  Controlador: `CharacterController { walk_speed: 4.317, air_control: 1, jump_speed: 9.5,
  step_height: 0.6 }` (`step_height` desde P5, ver 9c)
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

- **9. Entre entidades (P3).** Los sólidos se separan por posición en todos los ejes, con
  apilado, y cada empuje pasa por el barrido de vóxeles (las paredes siempre ganan). Los
  sensores avisan con `CollisionStarted` y `CollisionEnded`. Los sólidos estáticos y
  cinemáticos son obstáculos dentro del barrido de los dinámicos. Por partes:

  - **Broadphase (`core::grid::Grid`).** Grilla uniforme de celdas de `2^grid_shift` bloques
    (4 por defecto) con coordenadas `i64`. Cada cuerpo guarda sus `BlockBounds` (los bloques que
    alcanza su caja: `floor(min)..=floor(max)`, calculados en el marco de su bloque) y entra en
    cada celda que tocan, como mucho dos por eje; uno más ancho va a una lista `large` que se
    prueba contra todos. Las entradas se ordenan por un hash fijo de la celda con un counting
    sort (lineal y estable: el orden solo depende del orden de inserción). Un par es candidato si
    sus capas se aceptan en los dos sentidos, uno de los dos es activo (en P3: dinámico, sensor o
    con `ReportContacts`; desde P4: dinámico despierto, cinemático que se movió, o cuerpo
    teletransportado, movido a mano o con collider cambiado en el paso; dos paredes nunca forman
    par) y sus bloques se tocan; cada par sale solo de su celda "casa", la de `max(celda mínima
    de a, celda mínima de b)`, así que sale una vez. Los pares se ordenan por `Entity` (en P3 los
    cuerpos entraban en ese orden; desde P4 se ordenan solo los pares). El plan pedía
    `sort_unstable` con claves `i64`; el counting sort hace lo mismo en tiempo lineal. La misma
    grilla responde consultas de caja y rayos (un DDA exacto por celdas, igual al de vóxeles).
  - **Narrowphase (`core::narrow`).** En `f64`, en el marco del bloque del primer cuerpo, con el
    otro ubicado por `delta_to`: exacto a cualquier distancia. Caja contra caja por el eje de
    menor penetración (desempate Y, X, Z); esfera contra esfera por la recta entre centros (+Y si
    coinciden); esfera contra caja desde el punto más cercano (o la cara más cercana si el
    centro está adentro). Agregado al plan: dos cajas que se solapan después de un paso se
    separan hacia el lado del que vino una de ellas en ese paso (`box_box_moving`, con el
    desplazamiento del paso que guarda `PhysicsSync`). Sin esto, una caja que cae rápido y se
    hunde más de su altura en otra en un solo paso salía por abajo. Solapar es pasar de `eps` en
    todos los ejes: rozar un costado no cuenta.
  - **Solve.** Los pares sólidos con al menos un dinámico se recorren de abajo hacia arriba (por
    la base del cuerpo más bajo, desempate por par), `solver_passes` veces (2). Un cuerpo que no
    puede moverse en esa dirección (estático, cinemático, congelado, o dinámico que ya toca algo
    en la cara hacia la que lo empujarían) no recibe parte, y el otro se lleva todo el empuje:
    una pila de cajas sobre el suelo descansa sin hundirse. Dos dinámicos libres se reparten el
    empuje por `push_weight` (el doble de peso, la mitad de movimiento). El que se lleva todo el
    empuje queda exactamente sobre la cara del otro, calculado en su propio marco: una caja
    apoyada en otra da la misma posición bit a bit en cada paso. Cada empuje pasa por `move_box`
    (vóxeles y obstáculos); si el primero se mueve menos que su parte, el segundo se lleva el
    resto. La velocidad con que se acercan por la normal se quita, repartida igual (un choque
    inelástico). Cada cara que toca otro cuerpo se marca en `Contacts::faces`, y un empuje hacia
    arriba pone `on_ground` (se puede parar sobre una caja o un mob, y saltar desde ahí).
  - **Sensores y eventos.** Un sensor nunca empuja ni es empujado. Después de los empujes, los
    pares con un sensor que se solapan entran al conjunto de contactos del paso, junto con los
    pares sólidos con `ReportContacts` que se tocan o se solapan. En P3 contaban los que se
    solaparon antes de separarse; con eso nunca había eventos contra un estático o un
    cinemático (el barrido deja al dinámico apenas tocándolo) y un contacto terminaba en falso
    cuando el de arriba se dormía sobre uno despierto. Tocarse es estar a `eps` en un eje y
    solaparse más que `eps` en los otros dos (rozar una arista no cuenta); un estático o un
    cinemático cuenta como su caja, como lo ve el barrido. El conjunto se compara
    con el del paso anterior (merge de dos listas ordenadas): `CollisionStarted` para los nuevos
    y `CollisionEnded` para los que faltan, una vez cada uno, con `a < b` por bits de `Entity`.
    Un despawn da `CollisionEnded` en el paso siguiente. Un dinámico con collider sensor sigue
    chocando con los vóxeles (así funcionan los objetos tirados).
  - **Obstáculos.** Después de mover los cinemáticos, los colliders sólidos estáticos y
    cinemáticos forman otra grilla; cada dinámico sólido barre contra las cajas de los que
    estén en su camino y en sus capas (las esferas como su caja). Un cinemático que entra en un
    dinámico lo empuja todo el camino y le pasa su velocidad por la normal: una plataforma que
    sube lleva la caja encima.
  - **Capas.** `CollisionLayers { member, mask }`: dos colliders se encuentran si cada uno acepta
    al otro. Valen para pares, obstáculos y consultas.
  - **Consultas.** `BodyIndex` guarda los cuerpos con collider como los dejó el último paso
    (después de los empujes), en la grilla. `PhysicsQuery::raycast` tira el rayo de vóxeles y
    después busca cuerpos hasta el bloque que pegó (un cuerpo a la misma distancia gana: está
    delante de la cara); `overlap_box` da los cuerpos que se solapan con una caja;
    `block_is_free` dice si un bloque está libre de cuerpos sólidos (los sensores no cuentan).
    `voxel_world` no pone un bloque donde está el jugador.
  - Un `Collider` sin `RigidBody` es un cuerpo estático (lo requiere), como pide el plan. Un
    cuerpo sin `Collider` no participa entre cuerpos.

- **9b. Proyectiles, dormir y escala (P4).**

  - **Mensajes del mundo.** `boxel_world` suma `BlockChanged { pos, old, new }` (uno por bloque
    que cambió en el frame, escrito en `WorldSystems::Edits`) y `ChunkDataEvent::{Loaded,
    Unloaded} { id, key }` (cuando los bloques de un chunk se pueden leer o dejan de poder
    leerse; el mapa los anota al cambiar un slot y se publican al final de `Collect`). Son los
    mensajes que el plan pedía compartir con la luz. La luz no los necesita: calcula la luz en
    los jobs de malla, y la misma pasada de `apply_edits` que aplica las ediciones arma el lote
    de remallas que la recalcula (ADR 0015). Los `BlockChanged` salen de esa pasada, de la lista
    de cambios por vóxel que la luz lleva para medir el alcance de cada edición (antes de unir
    las ramas eran una lista propia de la física): por posición, el estado antes de su primera
    edición y después de la última, en el orden de la primera edición, sin las que quedan como
    estaban. En el frame en que se escribe, el mapa ya tiene el bloque nuevo, pero la luz
    guardada (`ChunkMap::light_at`) y las mallas pueden ser las de antes hasta que llegan las
    remallas del lote. La física lo lee en `FixedPostUpdate`: recibe todos los mensajes, pero
    los de frames sin paso fijo llegan después del `Update` siguiente, que pudo volver a
    editar el bloque o descargar su chunk. Por eso solo usa `pos` y lee el mapa.
  - **Dormir.** Un cuerpo dinámico se duerme (`Sleeping`) después de `sleep_steps` (32) pasos
    seguidos apoyado (en bloques o sobre otro cuerpo), más lento que `sleep_speed` (0.05
    bloques/s) en cada eje y sin moverse más que eso en el paso, con empujes incluidos. Un
    personaje que quiere caminar nunca cuenta como quieto (si no, uno que empuja una pared se
    dormía y despertaba cada 33 pasos). Al dormirse, un hook pone su velocidad en 0 exacto y su
    `Transform` en su posición. Un cuerpo dormido no se mueve, no se interpola y no entra en la
    lista del paso.

    Se despierta con: una velocidad distinta de 0 (un salto, un empujón, un controlador que
    quiere caminar), un cambio de su `RigidBody`, `Collider` o `BodySettings` hecho después de
    dormirse (comparando el tick del cambio con el de la marca: un cuerpo creado ya dormido no
    se despierta por sus propios componentes nuevos), un `ReportContacts` puesto o sacado, un
    collider quitado, un teletransporte o un `BodyPosition` escrito a mano, un empuje de un
    cuerpo despierto, un cuerpo sólido que toca y que se movió (la caja de abajo se fue), uno
    que se fue de donde lo tocaba (teletransporte, empujón a mano, collider cambiado o quitado,
    despawn), un `BlockChanged` a un bloque o menos de su caja, la carga de un chunk que toca su
    caja más un bloque (`ChunkDataEvent::Loaded`: los bloques pueden haber cambiado) y un cambio
    de `PhysicsSettings` (gravedad, dormir apagado). La descarga no lo despierta: dormido no se
    mueve y no necesita bloques. Antes de la revisión lo despertaba, el cuerpo se congelaba y
    ya no volvía a dormirse, así que los objetos que el jugador dejaba atrás costaban como
    despiertos para siempre. El plan pedía despertar con el bloque que cambia, la recarga del
    chunk, la velocidad, el empuje y el teletransporte; lo de la caja de abajo que se va hizo
    falta para que una pila no quede flotando.

    `Collider` es inmutable desde la revisión de P4: para cambiarlo se inserta otro, y su hook
    `on_discard` ve el que se va. Así una plataforma que se achica, se vuelve sensor o cambia de
    capas despierta lo que dormía encima; con `&mut Collider` el cambio no pasaba por ningún
    hook y la caja de arriba quedaba flotando. Un cuerpo creado con `Sleeping` (un objeto
    cargado de una partida guardada) queda dormido donde lo ponen su `Transform` o su
    `Teleport`; antes aparecía en el origen.

    Para que dormir ahorre de verdad, los dormidos viven aparte: un almacén con su propia
    grilla, que se rehace solo cuando se durmieron o despertaron más de `max(256, vivos / 8)`
    desde la última vez (los nuevos esperan en una lista corta `pending`, los despertados se
    marcan muertos, en la grilla y en `pending`, que se compacta de una vez al cambiar; antes
    sacar uno de `pending` corría el resto de la lista y despertar a todos costaba O(k²)). Los
    eventos de carga buscan sus dormidos en un índice de chunk a cuerpo que se arma con la
    grilla, en vez de recorrer todos los dormidos. Cada paso, los cuerpos activos (dinámicos
    despiertos, cinemáticos que se movieron, teletransportados, movidos a mano o con collider
    cambiado) buscan en esa grilla los dormidos que
    alcanzan y traen una copia a la lista del paso; el resto del almacén no se toca. En la
    grilla, las cajas de los activos que se movieron cubren de dónde vinieron, así un dormido
    que se apoyaba en ellos aparece aunque ya no se toquen. Los dormidos son inactivos: dos de
    ellos nunca forman par, y un par de dos inactivos que se solapaba sigue solapado (no hay
    `CollisionEnded` mientras los dos duermen). La marca `Sleeping` manda: sus hooks avisan
    cada vez que alguien la pone o la saca (despawns incluidos) y el almacén los sigue. Dormido
    se puede poner y sacar a mano.
  - **Proyectiles.** `Projectile { on_block: Stop | Bounce, on_entity: Stop | Pass, ignore }`
    sobre un cuerpo dinámico. Corren después del solve (los blancos están donde quedaron) en su
    propio set, `PhysicsSystems::Projectiles`: integran como cualquier cuerpo pero sin tope de
    velocidad (`StepParams::without_speed_limit`, con lo que `predict` da su vuelo libre bit a
    bit), y mueven su centro por el segmento del paso: contra los vóxeles con el DDA exacto, y
    contra los cuerpos sólidos de sus capas como un rayo contra la caja del blanco agrandada en
    las medias medidas del proyectil (exacto para cajas; una esfera cuenta como su caja). Un
    blanco donde el segmento empieza adentro o tocando cuenta solo en el primer paso después de
    crear, teletransportar o mover a mano el proyectil (un tiro a quemarropa); después no (ya se
    lo golpeó al entrar, o el proyectil se paró contra él). Gana el primer golpe; un cuerpo gana
    el empate con un bloque. `Stop` en un cuerpo lo deja tocándolo con velocidad 0; `Pass` avisa
    de cada cuerpo en el que entra, en orden, y sigue; en un bloque el centro queda sobre la
    cara (a `eps` de ella) y se clava (`Stop`: se duerme en el acto y recuerda el bloque,
    `Projectile::stuck_in`; si el bloque sigue ahí al despertar, se vuelve a dormir; si se fue,
    cae; si el juego le da una velocidad o lo mueve, vuela de nuevo) o rebota (`Bounce`, con las
    reglas de rebote de un cuerpo, y el resto del paso sigue a lo largo de la cara, así una
    pelota que cayó se desliza en vez de frenarse en seco). Cada golpe es un `ProjectileHit { projectile,
    target: Block { pos, face, state } | Entity(e), point, velocity, speed }`, ordenado por
    proyectil (un rebote solo si pasa de `impact_speed`). El plan pedía `speed`; se suma
    `velocity` para que el juego pueda empujar al blanco. Los proyectiles no están en
    `BodyIndex`: no empujan, no son empujados, no dan eventos de sensor ni se chocan entre ellos.
    Tampoco son obstáculos aunque su cuerpo no sea dinámico (un `Collider` solo lo haría
    estático: no vuela, y antes además bloqueaba a los dinámicos). Un segmento que llega a un
    chunk sin cargar congela al proyectil.
  - **Paso en paralelo.** Con más de `parallel_threshold` (256) cuerpos dinámicos despiertos
    (los estáticos, cinemáticos y dormidos no cuentan: casi no trabajan), el paso de
    los dinámicos y el de los proyectiles usan `par_iter_mut` en el pool de cómputo: cada
    cuerpo lee el mundo y escribe solo lo suyo, con su propio caché de chunks por hilo, y las
    salidas (impactos, golpes, congelados) se juntan y se ordenan por entidad. Lo mismo, porque
    son por cuerpo, el `Prepare`, el controlador y el `Writeback`. El broadphase calcula los
    límites de cada cuerpo en tandas por hilo y, desde 4096 cuerpos en la lista, reparte el
    barrido de pares de la grilla por rangos de celdas (`Grid::pair_ranges`, `pairs_in`,
    `large_pairs`). Para que el orden de la lista no importe, los pares se ordenan por entidad
    (antes se ordenaba la lista entera, que con 10 000 cuerpos de casi 300 bytes costaba 0.35
    ms). El solve seguía en un hilo; desde la issue #13 también va en los hilos (9d). Paralelo
    y en un hilo dan lo mismo bit a bit (test).
  - **stress.** `--bodies N --body-kind item|mob --projectiles M` ponen cuerpos alrededor del
    inicio cuando el mundo cargó (ver [benchmarks](../benchmarks.md)). Las corridas con ventana
    de la regla del frame quedan para la medición.

- **9c. Subir escalones (P5, issue #15).** Un personaje sube solo un escalón bajo, sin saltar,
  como en Minecraft: una losa o una escalera de losas sí, un bloque entero no.
  `CharacterController::step_height` es el escalón más alto (0.6 por defecto, el del jugador y
  los mobs de Minecraft; 0 lo apaga). El algoritmo es el de Minecraft, por ejes como el resto del
  barrido (`move_box_stepping` con `StepUp { height, grounded }`):
  - Solo se intenta si el movimiento normal choca de costado (X o Z), el cuerpo no empezó dentro
    de bloques y está en el suelo: terminó el paso anterior apoyado o este movimiento aterriza.
    En el aire nunca sube.
  - Se prueba el avance levantado de dos formas: sube `step_height` (recortado por lo que tenga
    encima) y avanza; y si algo sobre todo el camino (la caja estirada por el avance) está más
    bajo, sube solo hasta ahí y avanza. Así nunca se mete en un techo ni pasa por un hueco
    donde no entra.
  - Gana el que más avanza (el primero si empatan), y solo si avanza más que el movimiento
    normal. Después baja lo que subió, más el movimiento vertical del paso, hasta el escalón o
    donde aterrice: nunca queda más alto que el escalón. Informa las caras de costado del
    avance levantado y la de abajo de la bajada (`on_ground` sobre la losa), así que la
    velocidad de costado no se pierde.
  - Todo pasa en el marco del bloque de inicio: sigue exacto y bit a bit igual lejos del origen.
    Los obstáculos (sólidos estáticos y cinemáticos) se juntan sobre la caja subida por
    `step_height`, así subir a una caja estática ve lo que hay encima.
  - Solo lo pagan los personajes cuyo movimiento chocó de costado (dos a cuatro barridos más).
    Los cuerpos sin controlador no suben, y los proyectiles nunca (tienen su propio paso).
  - Descarte temprano (issue #13): un caminante que empuja contra una loma de un bloque pagaba
    esos barridos en cada paso sin subir nunca. Ahora no se prueban cuando se puede ver antes,
    sin error, que ningún avance levantado llegaría más lejos. Por cada eje de costado, en el
    orden del barrido: si el movimiento normal chocó ahí, tiene que ser contra un bloque entero
    (no un obstáculo) más alto que `step_height` sobre los pies y que empiece bajo la cabeza, a
    cualquier altura de las subidas posibles (de `eps` bajo el inicio hasta `step_height`
    arriba), y la caja tiene que estar tocándolo: a menos de media
    `CollisionTable::plane_gap` del plano (la distancia más chica entre una cara de caja de un
    bloque y el plano de bloque más cercano; 1/16 en los bloques de hoy), sin caras de
    obstáculos en el medio. Así ninguna otra cara cabe antes y todo avance levantado frena en el
    mismo plano. Si el eje iba libre, una caja que cubre todas esas alturas tiene que avanzar
    libre también (un recorte, casi siempre sin leer bloques). Entonces cada avance levantado
    termina de costado justo donde el normal, bit a bit, y la subida no gana: se descarta. Los
    márgenes de unos `eps` dejan el redondeo fuera de todas las comparaciones. El primer choque
    contra la loma (todavía sin tocarla) se prueba entero; los siguientes, empujando, ya no. Un
    escalón que se descarta así no lee los bloques de arriba: si estaban en un chunk sin cargar,
    el cuerpo ya no se congela por ellos (no podían cambiar el resultado).
  - Lo demás no cambia: entre dinámicos no hay escalón (el solve los separa como antes), un
    personaje dormido despierta al querer caminar y sube con los `Contacts` que tenía, y un
    escalón que necesita un chunk sin cargar congela al cuerpo como cualquier otro paso.
  - Cámara: el cuerpo sube media losa en un paso fijo (15.6 ms) y la cámara pegada al ojo daba
    un salto. Los ejemplos (`examples/shared/player.rs`) guardan un `EyeLag`: una subida
    estando apoyado y sin velocidad vertical deja el ojo abajo esa distancia, que se achica
    dividiéndola por `1 + 20 dt` cada frame (alcanza en una décima de segundo). Es solo la
    vista y queda en los ejemplos: el cuerpo sigue exacto y cada juego elige su cámara.

- **9d. 10 000 cuerpos despiertos (issue #13).** Con 10 000 mobs despiertos el paso no entraba
  en el frame: el broadphase y el solve iban casi todos en un hilo. Ahora, con más de
  `parallel_threshold` cuerpos despiertos:
  - Broadphase, desde 4096 cuerpos en la lista del paso: cada hilo arma su tramo de la lista y
    de la grilla (`GridSlot`, `Grid::push_slots`). Cada cuerpo va a su lugar fijo, el de la
    query, así que la lista, la grilla y los pares son los mismos que en un hilo. Las claves de
    entidad de los pares se arman una vez antes de ordenarlos.
  - Solve: los pares sólidos se parten en islas. Un empuje solo cambia los dos cuerpos del par,
    y solo los libres (dinámicos no congelados); los demás solo se leen. Las islas son los grupos
    de pares unidos por cuerpos libres (union-find), numeradas por su primer par. Cada isla
    conserva el orden de siempre (de abajo hacia arriba, `solver_passes` pasadas) y corre en un
    hilo sobre copias de sus cuerpos. Vuelven a la lista los libres que un empuje tocó (los
    demás no cambiaron): los mismos empujes, bit a bit, que una pasada por todos los pares.
    Desde 256 cuerpos que cambiaron, la escritura de vuelta a los componentes también va en los
    hilos (cada cuerpo escribe lo suyo).
  - El solve va en los hilos solo cuando paga. Lo que se reparte son los empujes: un par que
    solo se toca cuesta unos 25 ns en un hilo y casi el doble por islas (partir, copiar, las
    tareas). Y una pila o una multitud apretada es una sola isla, que en una tarea corre más
    lento que en el hilo del sistema (de 1.2 a 2.1 veces, medido). Por eso va en los hilos cuando
    el paso anterior empujó al menos 128 veces y ninguna isla tiene más de la mitad de los
    pares. Si una la tiene, se queda en un hilo y no vuelve a partir en 16 pasos (partir cuesta
    cerca de un décimo de los empujes). Antes se elegía por el tamaño de la lista (4096
    cuerpos), que no mide el trabajo del solve: pocos pares pagaban las tareas, y los empujes
    de 4096 mobs apretados iban 1.5 veces más lentos que en un hilo. `PhysicsStats::islands`
    dice cuántas islas fueron a los hilos en el último paso (0: un hilo). Mediciones en
    [rendimiento/fisicas-10k-2026-10](../rendimiento/fisicas-10k-2026-10.md).
  - Después de los empujes la grilla ya no se rehace entera: un cuerpo que cambió de celdas
    espera en la lista de grandes (se prueba contra todo) hasta el próximo armado, y las consultas
    lo ven en su lugar nuevo. Rehacerla costaba 0.11 ms por paso con 10 000 cuerpos.
  - `parallel_threshold = 0` corre todas las partes en los hilos aunque sean chicas; los tests
    lo usan.
  - Siguen en un hilo: contar y ordenar las celdas de la grilla, el orden de abajo hacia arriba
    de los pares, partir en islas (su trabajo sigue a los pares, no a la lista), los sensores,
    los eventos y el despertar.

- **9e. Vallas de 1,5 y modos del jugador (issues #46 y #37).** Las cajas de colisión pueden
  subir hasta media celda arriba (la valla choca a 1,5) y el barrido y el raycast las ven desde
  la celda de arriba. El controlador suma agacharse (sin caerse de los bordes), correr y el vuelo
  creativo. Detalle en ADR 0026.

- **10. Fases.** P0: requisitos (`WorldPos`, sets del bucle fijo, el tope de pasos, este ADR, la
  feature). P1: núcleo y raycast (tabla, fuentes, barrido, DDA, `PhysicsQuery::raycast_voxels`,
  edición de bloques en `voxel_world`). P2: cuerpos (gravedad, arrastre, rebote, congelar,
  interpolación, teletransporte, controlador). P3: entre entidades. P4: proyectiles, dormir y
  paso en paralelo. P5, a pedido: subir escalones (hecho, 9c), formas de bloque con cajas
  (hecho, ADR 0018), fluidos, fricción por bloque, esfera exacta contra vóxeles.

## Consecuencias

Hecho (issue #13, 10 000 cuerpos despiertos, 9c y 9d):

- `boxel_physics`: el descarte temprano del escalón (`CollisionTable::plane_gap`; la función
  oculta `move_box_stepping_unpruned` siempre prueba subir, para los tests y los benches);
  `GridSlot`, `Grid::slot`, `Grid::push_slot` y `Grid::push_slots`; `Grid::set_bounds` ya no
  pide rehacer la grilla; el broadphase, el solve (islas) y su escritura de vuelta en los hilos;
  el solve elige los hilos por sus empujes y por la isla más grande, y cuenta sus islas en
  `PhysicsStats::islands`; `parallel_threshold = 0` corre todo en los hilos.
- Tests: en `step_up.rs`, empujar contra una pared no lee el chunk de arriba (descartado) y sí lo
  lee sin el descarte; una proptest de 512 casos (se corrió también con 20 000) compara el
  barrido con y sin descarte, bit a bit, en campos de piedra, lomas y losas, con obstáculos, con
  cuatro movimientos seguidos (los siguientes empujan contra lo que frenó al primero); otro test
  cuida que esos campos den muchos choques. En `grid.rs`, una proptest: cuerpos movidos después de
  armar la grilla se encuentran en su lugar nuevo sin rehacerla (consultas, celdas, pares). En
  `threads.rs`, una multitud (360 cuerpos: caminantes, cajas que se duermen y despiertan, objetos
  sensores, pelotas, pilares, un cinemático, lomas y losas) da los mismos cuerpos y eventos bit a
  bit en un hilo y con todo el paso en pools de 1, 3 y 8 hilos (cada uno en un proceso hijo,
  porque el pool es global). Invertir el orden de los pares de una isla lo hace fallar (probado a
  mano). También en `threads.rs`, con el umbral de siempre: 200 torres de cajas que empujan van
  en los hilos (una isla por torre), una pared de 4128 cajas (una isla) y 4096 cajas que se tocan
  sin empujar se quedan en un hilo, y los cuerpos quedan igual bit a bit que en un hilo. Con la
  elección de antes (por el tamaño de la lista) los tres fallan.
- Benches nuevos: `sweep/step_wall` y `sweep/step_wall_slide` (un caminante que empuja contra una
  pared de un bloque, derecho y deslizándose) y sus `_tried` (sin descarte); `scale/4096_packed`
  (4096 caminantes apretados en 40 x 40 bloques: una sola isla).
- Costo (detalle en [rendimiento/fisicas-10k-2026-10](../rendimiento/fisicas-10k-2026-10.md); 4
  corridas intercaladas de cada versión con la máquina quieta, medianas): `scale/10000_awake`
  baja de 2.47 a 1.75 ms (29 % menos), `10000_awake_one_thread` de 6.24 a 5.87 ms,
  `10000_asleep_one_thread` de 0.783 a 0.746 ms y `tick/1000` de 0.486 a 0.479 ms; `grid/10000`
  y `10000_asleep` quedan igual. Ese día la máquina iba más lenta que en P4 (la base dio 2.47 ms
  donde la issue anotó 2.06 ms): a esa escala, 10 000 despiertos quedarían cerca de 1.46 ms, el
  objetivo era 1.5. `grid/10000` (0.49 ms, en un hilo) y `10000_asleep_one_thread` (0.75 ms)
  siguen sin llegar a sus objetivos (0.3 y 0.6 ms). El barrido sin escalón queda 4 % más lento
  (`sweep/player_walk` de 0.239 a 0.248 µs), por el reparto del código entre funciones; empujar
  contra una pared cuesta 0.48 µs en vez de 0.94 µs (`sweep/step_wall` contra
  `sweep/step_wall_tried`). Con la elección del solve por empujes, `scale/4096_packed` baja de
  8.17 a 7.70 ms y `scale/10000_awake` queda igual (4 corridas intercaladas, máquina
  compartida).

Hecho (P5, subir escalones, 9c):

- `boxel_physics`: `CharacterController::step_height`, `StepUp` y `move_box_stepping`;
  `move_body` recibe un `StepUp` (`step_body` no sube). `move_box` no cambia.
- `physics_sandbox`: T arma una escalera de losas adelante. Los ejemplos que caminan suavizan el
  ojo en cada escalón (`EyeLag`).
- Tests (`boxel_physics/tests/step_up.rs`): una losa se sube con todo el avance y queda exacto
  sobre ella; un bloque entero no (igual bit a bit al barrido sin escalón) y con `step_height`
  1 sí; una escalera de losas se sube de a medio bloque sin frenar; sin escalón con un techo
  bajo sobre la losa o sobre la cabeza, y sí con lugar; la segunda forma (subir solo hasta el
  techo del camino) con bloques y con cajas obstáculo; cajas estáticas de medio bloque se suben
  y de uno no; en el aire no sube, apoyado en el paso anterior o aterrizando sí; `step_height`
  0, negativo o NaN es el barrido de siempre; deslizar contra una pared no cambia; un escalón
  que necesita un chunk sin cargar da `Unloaded`. Una proptest sobre campos de cubos y losas:
  nunca termina dentro de un bloque, no sube más que el escalón, no avanza más de lo pedido y
  solo cambia el movimiento si avanza más. Con `App`: un personaje dormido despierta y sube una
  escalera sin saltar ni frenar, con `step_height` 0 la losa es una pared, bajar de una losa
  cae con la gravedad como antes, una caja sin controlador y una flecha con controlador no
  suben, y subir a una caja estática da lo mismo a 64 y a 144 fps. Lejos del origen (±2^31,
  2^40, 2^52 y el límite del mundo), el barrido y la subida con `App` dan lo mismo bit a bit.
  La escena de determinismo de `plugin.rs` suma dos tiras de losas. Sacar la segunda forma o la
  regla del suelo hace fallar estos tests (probado a mano).
- Costo, en la máquina compartida con otros agentes compilando (3 corridas intercaladas, con
  ruido): el barrido sin escalón queda igual o hasta 5 % más lento (`sweep/player_walk` de unos
  225 a 245 ns en las corridas más tranquilas); por eso la subida va fuera de línea (`#[cold]`).
  `tick/1000` sube de unos 0.47 a 0.49 ms. En `scale/10000_awake_one_thread` el movimiento sube
  de unos 3.5 a 4.0 ms: los caminantes del bosque chocan con lomas de un bloque casi cada paso y
  cada choque paga dos barridos más que no suben. Si molesta, el próximo paso es descartar
  antes los choques contra bloques enteros más altos que el escalón (hecho en la issue #13,
  ver 9c).

Hecho (P4):

- `boxel_world`: los mensajes `BlockChanged` y `ChunkDataEvent` (9b) y `DataState::is_readable`.
- `boxel_physics`: `Sleeping` y su almacén, `Projectile` con `OnBlock` y `OnEntity`,
  `ProjectileHit` con `ProjectileTarget`, los sets `PhysicsSystems::{Wake, Projectiles,
  Sleep}`, `PhysicsSettings::{sleep_speed, sleep_steps, parallel_threshold}`,
  `PhysicsStats::{sleeping, woken, projectiles, hits, parallel}` (la línea `physics` suma
  `sleeping`, `projectiles` y `hits`), `PhysicsSync::net_moved`,
  `StepParams::without_speed_limit` y, en la grilla, `pair_ranges`, `pairs_in` y
  `large_pairs`. El paso, el broadphase, `Prepare`, el controlador y el `Writeback` usan los
  hilos de cómputo desde 256 cuerpos.
- `physics_sandbox`: F dispara una flecha a 60 bloques/s (se clava en bloques; un mob al que
  le pega sale empujado y la flecha desaparece), B tira una pelota que rebota hasta quedarse
  quieta, M pone cuatro mobs que caminan solos. F3 suma la línea `hunt` (flechas, clavadas,
  pelotas, mobs, golpes).
- `stress`: `--bodies N --body-kind item|mob --projectiles M`; el resumen anota los cuerpos y
  `tools/bench_report.py` no mezcla corridas con cuerpos distintos.
- Benches nuevos en `crates/boxel_physics/benches/physics.rs`: `scale/10000_asleep`,
  `scale/10000_awake`, `scale/10000_awake_one_thread` y `scale/1000_projectiles`, con el
  desglose de cada paso (movimiento, entre cuerpos, resto). En la revisión:
  `10000_asleep_one_thread` y `1000_projectiles_one_thread` (los de los objetivos),
  `10000_asleep_chunk_events` y `sleep_churn`; los `tick` corren en un hilo.

Tests de P4:

- `boxel_world/tests/messages.rs`: un `BlockChanged` por bloque que cambió (dos ediciones del
  mismo bloque dan uno, las que no cambian nada o caen en chunks sin cargar no dan ninguno, en
  el orden de la primera edición aunque estén en varios chunks), y
  los `ChunkDataEvent` reproducen exactamente el conjunto de chunks legibles al cargar, al irse
  el viewer y al volver (con ids nuevos).
- `boxel_physics/tests/sleep.rs`: una caja se duerme exacta y no se mueve en 500 pasos; una pila
  de 4 se duerme sin cambiar un bit y sin pares; despiertan una velocidad (y no un 0), un
  controlador que quiere caminar o saltar, un teletransporte (por `Teleport` y por
  `Transform`), un jugador que camina contra una caja dormida (la empuja sin meterse), la caja
  de abajo que sale despedida, se despawnea o pierde el collider (la de arriba cae al suelo),
  una plataforma cinemática que sube y baja, un bloque a un bloque o menos (uno a tres no la
  despierta; sacar el bloque de abajo la deja caer), la descarga del chunk (sigue dormida, sin
  congelarse) y su vuelta (despierta y vuelve a dormirse; con 300 cajas en el índice de chunks
  también), dormir apagado, la gravedad cambiada; dormir a mano deja un cuerpo colgado hasta
  sacarle la marca; uno creado ya dormido queda donde lo puso su `Transform` o su `Teleport`;
  escribir `BodyPosition` despierta al dormido, una plataforma movida así deja caer la caja de
  arriba y empuja a la que encuentra; una plataforma que se achica o se vuelve sensor deja caer
  lo que dormía encima; quitar el collider de un sensor dormido, ponerle `ReportContacts` o
  cambiarle el `RigidBody` lo despierta; los solapes de sensores siguen mientras todos duermen
  y terminan cuando uno sale; rayos, `overlap_box` y `block_is_free` encuentran dormidos sin
  despertarlos; un despawn dormido no deja rastro; 400 cajas dormidas que se despiertan de a
  una dejan el índice exacto; `FixedPostUpdate` sin ambigüedades.
- `boxel_physics/tests/projectile.rs`: 64 flechas a 60 bloques/s, una por cada 1/64 de paso de
  fase, pegan las 64 a un mob de 0.6 de ancho y se paran justo en su cara (también con
  gravedad); una flecha se clava a `eps` de la cara del bloque, se vuelve a dormir si el bloque
  sigue y cae y se clava arriba de otro cuando se va; una pelota proyectil rebota y descansa;
  una flecha que atraviesa pega a tres mobs en orden y se clava en la pared; `ignore`, capas y
  sensores filtran; un vuelo libre es `predict` bit a bit y no tiene tope de velocidad; un chunk
  sin cargar la congela; un mob dormido recibe el golpe y despierta con el empujón del juego;
  una ráfaga de 300 proyectiles da lo mismo bit a bit lejos del origen (2^31, 2^52, ±(2^61 -
  1024)) y en varios hilos. Un tiro a quemarropa (creado dentro del mob) le pega una vez; una
  flecha clavada vuela de nuevo si se le da velocidad; una pelota que cae se desliza sobre el
  suelo.
- `boxel_physics/tests/contacts.rs`: la escena llena da lo mismo bit a bit con todo el paso en
  los hilos de cómputo, también lejos del origen. La pila de P3 corre con dormir apagado para
  seguir contando los empujes de cada paso. `ReportContacts` avisa de una caja sobre una
  plataforma estática y de un jugador contra una pared cinemática, y el contacto sigue cuando la
  caja se duerme sobre un personaje despierto; un proyectil sin cuerpo dinámico no es
  obstáculo; una consulta de caja infinita encuentra todos los cuerpos.
- `boxel_physics/tests/grid.rs`: proptest de las piezas del barrido en paralelo
  (`pair_ranges`, `pairs_in` en cualquier orden y `large_pairs`) contra `pairs`; consultas más
  anchas de lo que cuenta un `u128` recorren los cuerpos en vez de desbordar.
- `physics_sandbox` (sin ventana): F clava una flecha en una pared, F empuja a un mob y la flecha
  desaparece, B tira una pelota que rebota y se duerme apoyada, M pone mobs que caminan.
  `voxel_world`: el jugador quieto se duerme (la línea de física lo dice) y W lo despierta.
- `stress`: las opciones nuevas, objetos que caen y se duermen todos, mobs que siguen caminando y
  flechas que vuelven a volar y pegan.

Costo de P4 (detalle en [rendimiento/fisicas-p4-2026-10](../rendimiento/fisicas-p4-2026-10.md)),
medido de nuevo en un hilo después de la revisión, porque los objetivos del plan son en un hilo:
un paso con 10 000 cuerpos, 9000 de ellos dormidos, cuesta 0.637 ms (objetivo 0.6 ms: no llega
por 6 %; 0.447 ms con el paso en paralelo), y uno con 1000 flechas en vuelo 0.248 ms (objetivo
0.3 ms; 0.156 ms en paralelo). Con 10 000 cuerpos despiertos cuesta 2.08 ms en los hilos de
cómputo (4.76 ms en un hilo) y el objetivo era 1.5 ms: no se cumple. El movimiento ya va en
paralelo (0.49 ms); lo que queda es lo de entre cuerpos (1.4 ms), casi todo en un hilo: juntar
los cuerpos, meterlos en la grilla, los empujes y rehacer la grilla. `tick/1000` en un hilo baja
de 0.403 a 0.355 ms porque los objetos tirados se duermen (los 0.26 ms medidos primero eran con
el paso en paralelo); `tick/0` sube de 0.86 a 1.5 µs por los sistemas nuevos.

Cambios de API de P4:

- `PhysicsSystems` suma `Wake`, `Projectiles` y `Sleep`.
- `PhysicsStats::summary` suma `sleeping`, `projectiles` y `hits`; `dynamic` cuenta los cuerpos
  dinámicos despiertos del paso (congelados y proyectiles incluidos).
- `PhysicsSettings` suma `sleep_speed`, `sleep_steps` y `parallel_threshold`.
- `Collider` es inmutable y tiene un hook `on_discard`; `Sleeping` y `ReportContacts` tienen
  hooks `on_add` y `on_remove`: no se les puede registrar otros.
- `BodyIndex::entities` y `len` cuentan también los dormidos; `pairs` nunca da dos dormidos.
- `boxel_physics` depende también de `boxel_tasks`.
- `boxel_world::WorldPlugin` agrega dos mensajes; `ChunkMap` anota los cambios de datos.
- El ejemplo `stress` pide la feature `physics`.

Hecho (P3):

- `boxel_physics::core`: `narrow` (`Contact`, `box_box`, `box_box_moving`, `sphere_sphere`,
  `sphere_box`, `box_sphere`, `shape_contact`, `shape_box`) y `grid` (`Grid`, `BlockBounds`,
  `CollisionLayers`, `CellRay`). `move_body` es la mitad del integrador que mueve y rebota, con
  obstáculos (`step_body` la usa sin obstáculos).
- Capa ECS: `BodyIndex`, `BodyFilter`, `BodyHit`, `RayFilter`, `RayHit`, `ReportContacts`,
  `CollisionStarted`, `CollisionEnded`; `PhysicsQuery::{raycast, overlap_box, block_is_free,
  bodies}`; `PhysicsSettings::{solver_passes, grid_shift}`; `PhysicsStats::{pairs, pushes,
  overlaps, solve_time}` (la línea `physics` los muestra); `PhysicsSync::moved`.
- `physics_sandbox`: C tira una caja sólida de 0.75 (peso 2) dos bloques adelante, y las cajas
  se apilan. El jugador levanta los objetos que toca (su `CollisionStarted` con el sensor del
  objeto; uno tirado con Q, recién después de 1 s). F3 muestra objetos levantados y cajas.
- `voxel_world`: clic derecho no pone un bloque donde hay un cuerpo sólido (el jugador).

Tests de P3:

- `boxel_physics/tests/shapes.rs`: separación de caja contra caja, esfera contra esfera y esfera
  contra caja contra respuestas analíticas (desempates, tocar no cuenta, NaN), `box_box_moving`
  (hundida de más de media altura, apoyada, en diagonal, rozando un costado), y proptests:
  mover por el vector deja las formas tocándose y nada menos alcanza.
- `boxel_physics/tests/grid.rs`: proptest de pares contra fuerza bruta O(n²) con cuerpos chicos,
  anchos y grandes, capas y activos al azar, cualquier tamaño de celda y lejos del origen, antes
  y después de mover cuerpos; los mismos pares en cualquier lugar del mundo; consultas de caja
  contra fuerza bruta; el recorrido de celdas de un rayo igual al DDA de vóxeles con celdas de 1
  bloque, y con celdas de 4 cubre todos los bloques del DDA.
- `boxel_physics/tests/far_coords.rs`: pares, contactos, rayos y consultas de una escena de
  cuerpos corrida por 0, ±2^31, 2^40, 2^52 y ±(2^61 - 64): idénticos bit a bit.
- `boxel_physics/tests/contacts.rs` (una `App` sin ventana): una pila de 4 cajas descansa
  exacta y no cambia un bit en 1000 pasos; una caja que cae a 60 bloques/s queda encima, no
  atraviesa; empujes en X y Z repartidos por peso; un empuje nunca mete un cuerpo en una pared;
  un cinemático empuja una caja todo el camino y una plataforma que sube la lleva; colliders
  estáticos como obstáculos (y los sensores y otras capas los atraviesan); un sensor avisa una
  vez al entrar y una al salir, y un despawn termina el contacto; `ReportContacts`; capas que no
  se aceptan se atraviesan; rayos contra cuerpos y bloques en orden, `max_t`, esferas exactas,
  empezar adentro, `overlap_box` y `block_is_free`; un índice vacío antes del primer paso. Una
  escena llena (mobs que deambulan, pilas de cajas, pelotas, objetos, un cinemático, un pilar y
  una zona sensor entre paredes) da el mismo estado y los mismos eventos bit a bit en dos
  corridas, a 64 o 144 fps, con o sin interpolación, y corrida por 2^31, 2^40, 2^52 y
  ±(2^61 - 1024).
- `physics_sandbox` (sin ventana): tres cajas tiradas en el mismo lugar quedan apiladas exacto, y
  caminar hasta un objeto lo levanta. `voxel_world`: con el jugador caminando, clic derecho no
  pone piedra en sus pies; volando, sí.
- Probado a mano: cambiar `box_box_moving` por `box_box`, el empuje exacto por la profundidad, o
  el manejo de la casa y de la lista `large` de la grilla hace fallar estos tests.

Costo de P3 (detalle en [rendimiento/fisicas-p3-2026-10](../rendimiento/fisicas-p3-2026-10.md)):
`tick/1000` sube de 0.23 a 0.40 ms (objetivo 0.5 ms) y `tick/100` de 24 a 33 µs (objetivo 50 µs):
ahora incluye broadphase, solve y eventos, y los caminantes se empujan entre ellos. La grilla con
1000 cuerpos cuesta 25 µs. Con 10 000 cuerpos tan repartidos como los del tick cuesta 0.48 ms, y
el objetivo del plan era 0.3 ms: no se cumple. Con 10 000 apretados en 128 x 128 bloques cuesta
1.2 ms.

Cambios de API de P3:

- `CollisionLayers` vive en `core::grid` (sigue en la raíz del crate) y suma `new`.
- `Collider` requiere `RigidBody::Static`.
- `PhysicsQuery` lee también `BodyIndex`.
- `PhysicsStats::summary` suma `pairs`, `pushes`, `overlaps` y `solve`.
- `Contacts::faces` y `on_ground` también cuentan los contactos con otros cuerpos.

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

Lo que falta: medir la regla del frame con stress en ventana (`--bodies 1000` y `10000`, en las
dos GPU; con 10 000 mobs despiertos otra vez después de la issue #13, los comandos están en
[rendimiento/fisicas-10k-2026-10](../rendimiento/fisicas-10k-2026-10.md)), la grilla de 10 000 a
0.3 ms en un hilo (una grilla incremental, o un cuerpo en la grilla más chico) y lo que queda de
P5 (fluidos, fricción por bloque, esfera exacta). Los
cuerpos no tienen dibujo: hace falta un renderer de entidades.
`physics_sandbox` todavía no rompe ni pone bloques. La luz no usa `BlockChanged` ni
`ChunkDataEvent` (se recalcula con las remallas), pero el mensaje sale de su lista de cambios.

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
- Agregar `BlockDef::collision` en P1, sin formas que lo usen: se esperaba a la luz de colores
  para romper `BlockDef` una vez, pero la luz entró antes. Ahora llega con las formas de P5.
