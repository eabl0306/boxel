# ADR 0026: vallas de 1,5 bloques, modos del jugador y contorno del bloque apuntado

- Estado: aceptado (issue #46 y la parte del motor de la issue #37)
- Fecha: 2026-10-09
- Revisa: ADR 0016 (cajas de colisión que suben a la celda de arriba, modos del controlador) y
  ADR 0020, sección 6 (la valla choca a 1,5 bloques, no a 1)

## Contexto

- **Vallas.** En Minecraft una valla choca a 1,5 bloques de alto: el salto (1,24 bloques) no
  alcanza para pasarla. Acá chocaba a 1 bloque, porque el barrido y el raycast de `boxel_physics`
  suponían que las cajas de un bloque quedan dentro de su celda (ADR 0020, sección 6).
- **Jugador.** El clon necesita un jugador como el de Minecraft. Había caminar, saltar y subir
  escalones. Faltaban agacharse sin caerse de los bordes, correr, el vuelo creativo con doble
  salto y el contorno del bloque apuntado. Hoy el bloque apuntado solo sale en el texto de F3 y
  hay una mira en el centro.

## Decisión

### 1. Cajas de colisión que suben a la celda de arriba

- `CollisionBox::is_valid_collision`: como `is_valid`, pero en `y` la caja puede llegar hasta
  `MAX_COLLISION_HEIGHT` = 24 dieciseisavos (media celda arriba). En `x` y `z` sigue hasta 16, y
  la caja empieza en su celda (`min.y < 16`). El registro valida así la colisión. Las cajas de
  modelo y los contornos siguen dentro de la celda (`is_valid`).
- La valla tiene dos formas: `fence_collision`, de 24 de alto, y `fence_outline`, de 16. La
  primera es su colisión (`BlockModel::default_collision`) y la segunda su contorno para apuntar
  y dibujar (`BlockModel::outline`). Las cajas de dibujo no cambian.
- `collision_face_cover`: una caja que cruza la cara de arriba de su celda la cubre con su corte,
  como Minecraft corta una forma en la cara. Así el poste sigue sosteniendo una antorcha, y lo
  que sobresale no cuenta para ninguna cara.
- `CollisionTable` guarda un bit `TALL` por estado (`is_tall`) y `reach()`: lo más alto que sube
  una caja sobre su celda (0,5 con las vallas; 0 sin bloques altos).

### 2. Barrido

- **De costado (X o Z).** La huella suma la fila de bloques de abajo cuando una caja alta de esa
  fila puede llegar a la caja que se mueve (`fila + reach > base + eps`). En esa fila solo cuentan
  las cajas de los bloques altos: un bloque lleno de abajo es el piso, no algo en el camino. Es lo
  que hace Minecraft: `BlockCollisions` recorre desde `minY - 1` y en el borde solo toma las
  formas grandes (`hasLargeCollisionShape`).
- **Hacia abajo.** El techo de una valla puede quedar hasta `reach` arriba del techo de su capa,
  así que "la primera capa con un plano decide" ya no alcanza. Cuando una capa da un plano, se
  mira una capa más si su techo posible (`capa + reach`) lo puede superar. Como `reach` es menor
  que un bloque, es a lo sumo una capa más. El final del recorrido también baja `reach`.
- **Dentro de una capa**, hacia abajo, pasa lo mismo: un bloque lleno ya no corta la lectura,
  porque una valla de la misma capa puede tener el techo más alto. Solo corta un techo de
  `capa + 1 + reach`. Sin esto, con una piedra al lado de una valla, el cuerpo aterrizaba dentro
  de la valla o no, según de qué lado venía.
- **Hacia arriba** no cambia: las cajas altas no bajan de su celda.
- **Salida de un bloque.** `any_solid` mira la misma fila. Un cuerpo dentro de la mitad de arriba
  de una valla (una valla puesta bajo sus pies) sube al plano entero siguiente, como con las
  losas.
- **Escalón.** El algoritmo no cambia. Desde el suelo una valla mide 1,5, más que 0,6, y nunca se
  sube. Desde un bloque más alto su mitad de arriba (0,5) sí se sube, como en Minecraft. El
  descarte temprano sigue valiendo: solo poda contra bloques llenos, y las caras de las cajas
  altas cumplen la misma distancia mínima a los planos (`plane_gap`).
- Sin bloques altos (`reach = 0`) todo da lo mismo bit a bit que antes.

### 3. Raycast

- Con `VoxelFilter::Collision` y bloques altos en la tabla, en cada celda que el rayo cruza a
  `reach` o menos de su piso se prueba también el bloque alto de abajo. Un impacto se guarda
  hasta que la próxima celda empieza más lejos: una celda posterior solo puede dar algo más cerca
  por la mitad de arriba de una valla. Sin bloques altos, o con otro filtro, corre el bucle de
  siempre.
- Si el bloque de abajo está en un chunk sin cargar, el rayo termina (`RayResult::Unloaded`)
  donde entra en su alcance, hasta medio bloque arriba de él, y no donde entra en el bloque. Si
  antes de ese punto ya pegó en algo, devuelve ese impacto.
- El punto del impacto sigue exacto sobre el plano, también en el techo de la valla (y = 1,5:
  el bloque de arriba con fracción 0,5).
- Los proyectiles se apoyan en ese punto, con su fracción. Antes tomaban solo el bloque y
  quedaban dentro de una losa o medio bloque dentro de una valla, y una pelota se clavaba en vez
  de rebotar.
- `VoxelFilter::Outline` (apuntar) no cambia: el contorno de la valla es de un bloque.

### 4. Modos del controlador

- `MoveIntent` suma tres teclas mantenidas, que no son pestillos: `up`, `sneak` y `sprint`.
- `CharacterController` suma `sneak_factor` (0,3), `sprint_factor` (1,3), `fly_speed` (10,92
  bloques/s), `fly_vertical_speed` (7,5 bloques/s) y `flying`. Son los valores de Minecraft.
- **Agacharse.** La velocidad es 0,3 de la de caminar y gana a correr. En el suelo y sin saltar,
  el paso pasa antes por `guard_edge`, la guarda de bordes de Minecraft
  (`Player::maybeBackOffFromEdge`), pero exacta:
  - Un soporte es lo que la caja tocaría (más que `eps`) si bajara la caída (`sneak_drop`: el
    escalón, al menos 1/16): bloques, cajas de bloques (también las altas de la fila de abajo) y
    cuerpos estáticos y cinemáticos.
  - Primero X y después Z, cada uno solo: si el movimiento dejaría la caja sin soporte, se corta
    donde la caja guarda `EDGE_MARGIN` (2^-10 bloques) del último soporte. Nunca pasa de lo
    pedido ni va hacia atrás.
  - Después los dos juntos: si en esa diagonal no hay soporte (una esquina entre dos soportes),
    ningún eje se mueve. Es lo que da el acortado de Minecraft a velocidad de caminar.
  - En un eje cortado la velocidad queda en 0 después del paso.
  - Sin soporte donde está (en el aire, o parado sobre un cuerpo dinámico) no hay guarda. Un
    borde de hasta el escalón de alto (una losa) se baja.
- **Correr.** 1,3 de la velocidad, también en el aire.
- **Volar.** Sin gravedad y sin escalón. De costado `fly_speed` (el doble corriendo) con el
  arrastre del aire; `fly_vertical_speed` hacia arriba con `up` y hacia abajo con `sneak`, con un
  arrastre vertical fuerte (`FLIGHT_VERTICAL_DRAG`, 40/3 por segundo: Minecraft conserva 0,6 de
  la velocidad vertical por tick), así que frena enseguida. El salto no hace nada. Aterrizar (el
  barrido toca el suelo bajando) apaga el vuelo, como en Minecraft. Prenderlo parado no aterriza:
  sin gravedad el cuerpo no baja.
- Todo en el marco del bloque del cuerpo y solo con `+ - * /`, `floor`, `min` y `max`: el mismo
  resultado bit a bit lejos del origen.

### 5. Contorno del bloque apuntado

- Recurso `BlockOutline` (`boxel_render`): `block`, el color y el ancho en píxeles (2). El juego
  pone el bloque apuntado. El plugin lee su estado cada frame, así el contorno sigue al bloque
  (una valla que se conecta, una losa que pasa a doble).
- El color por defecto es negro al 67 %. Minecraft usa negro al 40 %, pero mezcla sobre valores
  sRGB y deja 0,6 de cada uno. Acá el destino es sRGB y la mezcla es lineal: con 40 % la línea
  saldría un tercio más clara, y 67 % da el mismo 0,6 (a 4 niveles de 255 o menos, de 40 a
  240).
- Las aristas (`block_outline_edges`) salen del contorno del modelo: la unión de sus cajas en una
  grilla de dieciseisavos, con una arista donde la superficie dobla. Así una escalera se dibuja
  como una sola forma, como en Minecraft. Se guardan por estado.
- Dibujo: la CPU arma, por arista, un quad de 2 píxeles de ancho de cara a la pantalla, en
  espacio de clip, cortado en el plano cercano y acercado al ojo 1/1024 de su distancia (Minecraft
  escala la vista por 1 - 1/4096) para ganar la prueba de profundidad contra las caras donde
  apoya. Un pipeline sin bindings (sin uniform ni textura), con prueba y escritura de
  profundidad. El tier Low no cambia sus límites (ADR 0011).
- Va en el pase principal después de los buckets opacos y cutout y antes de los translúcidos: las
  aristas tapadas quedan tapadas y el agua de adelante lo tiñe. Para eso `TierPath::draw` se
  partió en `draw_solid` y `draw_translucent`; la parte translúcida vuelve a poner sus buffers de
  vértices y sus immediates.
- Paridad: los tres tiers usan el mismo pipeline y los mismos vértices, así que dan los mismos
  píxeles. No cuenta en `RenderStats::draws`.

### 6. `voxel_world`

- Caminando: Shift se agacha, Ctrl corre y Espacio dos veces en 0,35 s prende o apaga el vuelo
  (Espacio sube, Shift baja, aterrizar lo apaga). Con la cámara libre, Shift y Ctrl siguen bajando
  y acelerando la cámara.
- F3 suma al modo `flying`, `sneaking` o `sprinting`.
- El bloque apuntado lleva el contorno, además de la mira.

## Consecuencias

- Costo, sin medir (hoy no se mide; va a la próxima ronda de control):
  - Barrido: un movimiento de costado con la base del cuerpo a menos de medio bloque sobre un
    plano entero lee una fila más de bloques. Un caminante sobre el suelo lo hace siempre: una o
    dos lecturas más por capa recorrida. Hacia abajo, la capa donde aterriza se lee entera (de 1
    a 4 bloques para un jugador) en vez de cortar en el primer bloque lleno. Camino caliente de
    `sweep/player_walk`, `tick/*` y `scale/*`.
  - Raycast con `Collision`: una lectura más por celda cruzada en su mitad de abajo. Camino
    caliente de los proyectiles (`scale/1000_projectiles`).
  - La guarda de bordes solo corre para quien se agacha en el suelo.
  - El contorno: un draw y unos 100 vértices por frame, armados en CPU.
  - Las mallas no cambian.
- Chunks sin cargar: un barrido de costado con la base a menos de medio bloque sobre un plano
  entero, o uno hacia abajo que puede llegar a medio bloque de un techo, necesita también la
  fila de abajo. Un cuerpo en el borde de abajo de un chunk cargado, con el de abajo sin cargar,
  se congela antes que hoy. Igual se congelaba al caer: su piso está en ese chunk.
- Cambios de API:
  - `CharacterController` y `MoveIntent` tienen campos nuevos: un literal sin `..default()`
    deja de compilar.
  - `CollisionBox::is_valid_collision`, `MAX_COLLISION_HEIGHT`, `shapes::fence_outline`.
  - `CollisionTable::{is_tall, reach}`, `guard_edge`, `EDGE_MARGIN`, `FLIGHT_VERTICAL_DRAG`.
  - `RayResult::Unloaded::t` con bloques altos (sección 3).
  - `BlockOutline`, `Outline`, `VoxelRenderer::set_outline`, `outline_edges`,
    `block_outline_edges` y `ShaderModuleId::Outline`.
- Tests:
  - `boxel_voxel`: la valla choca a 24 y se apunta a 16, su techo sostiene una antorcha y ninguna
    cara es firme; el registro acepta 24 y rechaza 25, una caja más ancha que la celda y una que
    empieza arriba.
  - `boxel_physics/tests/fences.rs`, en cinco bases (0, ±2^31, 2^40 y cerca del límite del
    mundo): la valla frena a un caminante, con escalón o sin él, y a una caja a 1,4375 de alto (no
    se salta); en 1,5 pasa por arriba y camina sobre la fila sin engancharse. Desde un bloque más
    alto, el techo es un escalón. Se cae sobre el poste a 1,5, un cuerpo dentro sube al plano
    siguiente. Con una piedra al lado de la valla, en la misma capa y desde los cuatro lados, el
    escalón, la caída y un paso de gravedad terminan en el techo de la valla, sin tocarla. Los rayos de colisión pegan en la mitad de arriba (cara, `t` y punto exactos) y
    los de apuntar no. Un chunk sin cargar bajo la celda corta el rayo y el barrido, salvo
    que el rayo ya haya pegado antes en un bloque. En `sweep.rs`, el test de los obstáculos que
    frenan el recorrido antes de un chunk sin cargar sube la caja medio bloque por esto.
  - `block_shapes.rs`: los oráculos de fuerza bruta (barridos por eje y rayos al azar) ahora
    tienen vallas en sus campos. El de barridos suma campos densos (densidad 0,3), donde bloques
    de todo tipo comparten capa.
  - `boxel_physics/tests/projectile.rs`: flechas y pelotas que caen sobre postes de valla y
    losas quedan en su techo (las pelotas rebotan y descansan ahí, sin clavarse), igual bit a bit
    en dos bases lejanas.
  - `boxel_physics/tests/player.rs`: la guarda corta en el borde con el margen exacto, no avanza
    ni retrocede desde el margen, corta las dos en una esquina, deja bajar una losa y no un
    bloque, no hace nada sin soporte o sin caída, toma cajas estáticas y techos de valla como
    soporte. Con `App`: caminar, correr y agacharse llegan a sus velocidades; agachado frena en el
    borde de un pozo de 3 sin bajar nunca y sin agacharse cae; agachado baja a una losa. Volar:
    prendido parado no aterriza, sube a 7,5, frena y queda quieto exacto, cruza al `fly_speed` (el
    doble corriendo), baja y aterriza, y se apaga. Todo igual bit a bit en cinco bases.
  - `boxel_render`: aristas de un cubo, una escalera (una forma) y una valla; los quads son los
    mismos en cualquier lugar del mundo y se cortan en el plano cercano.
    `tests/gpu_outline.rs`: en cada tier, el contorno se ve y solo oscurece (con el color por
    defecto, a 0,6 del valor sRGB en la mediana, como en Minecraft), el de un bloque tapado no
    cambia ningún píxel, sacarlo vuelve a la imagen sin él, no cuenta draws, y todos los tiers
    dan los mismos píxeles (AMD 780M, RTX 5050 en Vulkan y GL, llvmpipe: diferencia 0 %).
  - `boxel_shaders`: el overlay y el contorno no tienen recursos globales.
  - `voxel_world` (sin ventana): el bloque apuntado se contornea y fuera de alcance no; Ctrl corre
    y Shift se agacha con F3; agachado no cae al pozo y sin agacharse sí; Espacio dos veces vuela,
    Espacio sube, queda quieto, Shift baja y aterrizar camina, y dos toques en el aire lo apagan.

## Fuera de esta parte

- Muros de piedra y puertas de valla (también chocan a 1,5).
- La cámara en primera persona pulida (balanceo, FOV al correr) y la interacción, de la issue #37.
- Volar en el agua: `SwimPlugin` sigue tomando el pestillo del salto en un fluido.
- El impulso del salto corriendo, que correr se corte al chocar y el hambre.

## Alternativas descartadas

- Mirar siempre la fila de abajo con todos sus bloques: el piso frenaría cualquier movimiento.
- Partir la valla en dos bloques, con uno invisible arriba: ocupa una celda que el jugador puede
  usar y complica la edición.
- Líneas de 1 píxel (`LineList`): muy finas en pantallas grandes, y la rasterización de líneas
  cambia entre backends. Los quads de la CPU dan lo mismo en los tres tiers.
- Armar las líneas en el shader con el uniform `View`: el pipeline necesitaría el grupo de la
  vista en cada tier. Con vértices de CPU no hay bindings.
- Dibujar el contorno en otro pase después del translúcido: es más simple, pero el agua de
  adelante no lo teñiría y cuesta un pase más.
- La guarda de bordes de Minecraft con pasos de 0,05: depende de la velocidad y de la frecuencia
  de pasos. La exacta no.
