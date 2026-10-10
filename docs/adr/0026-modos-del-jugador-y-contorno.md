# ADR 0026: vallas de 1,5 bloques, modos del jugador y contorno del bloque apuntado

- Estado: aceptado (issue #46 y la issue #37; la sección 8 cierra el jugador)
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
- **Agacharse.** La velocidad es 0,3 de la de caminar y gana a correr. La pose agachada (caja
  de 1,5 de alto) está en la sección 8. En el suelo y sin saltar,
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
- **Correr.** 1,3 de la velocidad, también en el aire. Cuándo empieza y termina, en la sección 8.
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

### 6. `voxel_world` y `physics_sandbox`

- Caminando: Shift se agacha, Ctrl corre y Espacio dos veces en 0,35 s prende o apaga el vuelo
  (Espacio sube, Shift baja, aterrizar lo apaga). Con la cámara libre, Shift y Ctrl siguen bajando
  y acelerando la cámara.
- F3 suma al modo `flying`, `sneaking` o `sprinting`.
- El bloque apuntado lleva el contorno, además de la mira.
- `physics_sandbox` usa el mismo `WalkPlugin`, así que tiene las mismas teclas para caminar,
  agacharse, correr y volar. Su ayuda (`--help`) y sus docs las nombran.
- `snapshot --outline X,Y,Z` dibuja el contorno de ese bloque con el color y el ancho por
  defecto, para ver el contorno en capturas sin ventana.

### 7. Leer abajo solo cerca de bloques altos

Basta con que el registro tenga una valla para que `reach` valga 0,5 en todo el mundo, aunque no
haya ninguna cerca. Así, cada barrido y cada rayo de colisión pagaban las lecturas de más de las
secciones 2 y 3 en todos lados (con criterion, contra cf11b4f: `raycast/sky_1024` +79 %,
`tick/1000` +15 %).

- `ChunkData::may_contain(&StateSet)` mira la paleta del chunk. Puede decir que sí de más (un
  estado que quedó en la paleta sin uso), nunca que no de más. Guarda la respuesta para el último
  conjunto preguntado y la borra con cualquier cambio de bloques (`set`, `blocks_mut`,
  `compact`). No hace falta mantener una marca a mano en cada camino de escritura (generación,
  ediciones, reglas de vecinos, ticks de bloques, cargas): todos pasan por esos métodos.
- `VoxelSource::tall_free(chunk, table)`: el chunk está cargado y no tiene bloques altos. Por
  defecto dice que no y se lee todo, como antes. `ChunkSource` y `TestWorld` responden con
  `may_contain` y `CollisionTable::tall_states`.
- **Barrido.** Pregunta una vez por los chunks que puede leer: la caja al inicio, al final y
  subida por el escalón, más la fila de abajo y unos `eps`. Casi siempre es el chunk del bloque
  de inicio. Si están libres, corre una copia del código sin bloques altos (`Grid` sin `TALL`),
  igual a la de antes de las vallas. Si la caja empieza dentro de bloques, la salida la saca de
  esa zona, así que vuelve a empezar con `TALL`.
- **Guarda de bordes.** Pregunta por la fila de abajo de su zona.
- **Rayo de colisión.** Corre el bucle de siempre mientras su celda y el bloque de abajo están
  en chunks libres. Mira al entrar en otro chunk, en la misma comparación que el límite del
  mundo, así que no cuesta nada por paso. Si el chunk puede tener bloques altos, sigue con el
  bucle de la sección 3, que tampoco lee abajo en chunks libres. También sigue con ese bucle,
  desde esa celda y con el choque ya guardado, si choca con una caja después de la salida de la
  celda. Pasa por redondeo: la caja divide y el paso del rayo multiplica por el inverso, así que
  una cara sobre el plano de salida puede quedar un ulp más allá. La celda siguiente puede tener
  un choque más cerca (un bloque lleno o un chunk sin cargar) y ese bucle lo encuentra.
- El resultado es el mismo bit a bit, con vallas o sin ellas.

### 8. El resto del jugador (issue #37)

**Agacharse con la caja.** Como Minecraft desde la 1.14: agachado, el cuerpo mide 1,5 y el ojo
baja de 1,62 a 1,27.

- `CharacterController::crouch_height` (1,5) y el componente `Crouch`, que el controlador exige
  (todo personaje lo tiene). `Crouch` guarda el `Collider` de pie mientras el cuerpo está
  agachado; `is_crouching()` lo dice.
- La pose cambia en el paso fijo, antes de mover el cuerpo. Es el orden de Minecraft, que la
  cambia al final del tick anterior (`updatePlayerPose`). Si el personaje se agacha (Shift y sin
  volar), su caja pasa a `Collider::crouched`: el mismo piso y la misma huella, con el techo a
  `crouch_height`. Si deja de agacharse, se para solo donde la caja de pie entra: se barre la
  caja agachada hacia arriba hasta el techo de pie, contra los bloques y los cuerpos estáticos y
  cinemáticos, y no tiene que chocar ni empezar dentro de algo. Ese movimiento ya usa la caja
  nueva, y el `Collider` nuevo se inserta después del paso (antes de los choques entre cuerpos,
  en el punto de sincronización que ya había).
- Para reunir obstáculos, un cuerpo agachado usa su caja de pie: el barrido de pararse los
  necesita. Uno de pie no cambia nada.
- Donde no hay lugar para pararse sigue agachado, y lento, aunque suelte Shift. Así sale de un
  túnel de 1,5 sin quedar trabado. La guarda de bordes sigue solo con Shift, como en Minecraft
  (`isShiftKeyDown`).
- Un volador no se agacha (Shift lo baja). Una esfera, o un `crouch_height` que no está entre 0
  y la altura de la caja, no se agacha: Shift solo frena y cuida los bordes.
- Sueño: un cuerpo dormido no da pasos. Si su `MoveIntent` cambia y la pose tiene que cambiar
  (Shift parado, o soltarlo agachado), `wake_to_pose` lo despierta en ese paso. Un cuerpo agachado
  sin Shift (esperando lugar) no se duerme: prueba pararse en cada paso. Así se para apenas
  rompen el bloque de arriba.

**Correr.** `CharacterController::sprinting` es un estado, como `flying`. Es el sprint de
Minecraft (`LocalPlayer::aiStep`):

- Empieza con `MoveIntent::sprint` mientras el personaje avanza y no está agachado. Sigue
  aunque se suelte la tecla, como Minecraft con la tecla de correr en modo mantener.
- Avanzar se mide contra `MoveIntent::facing`, la dirección a la que mira el personaje: el
  deseo tiene que ir hacia adelante (`hasForwardImpulse`: W, W con A o con D; no A sola ni S).
  Con `facing` en cero cuenta la dirección del deseo, así una IA que no mira a ningún lado igual
  corre.
- Termina si deja de avanzar (para, retrocede o va de costado), si se agacha o si choca de frente.
  Chocar de frente es un corte de costado en el barrido que deja el movimiento a más de 8 grados
  del deseo, o casi quieto (menos de 0,063 bloques por segundo). Es el `horizontalCollision &&
  !minorHorizontalCollision` de Minecraft, con su ángulo. Rozar una pared en un ángulo chico no
  lo corta. Se calcula con productos escalares, sin `acos` ni `sqrt`.
- El choque queda en `Contacts::ran_into` para el paso siguiente. Ahí, como en el `aiStep` de
  Minecraft, la tecla lo empieza y el choque del movimiento anterior lo corta, antes de calcular
  la velocidad y el salto. Así, con la tecla apretada contra una pared, el cuerpo camina a lo
  largo de ella y su salto no suma el empujón.
- Subir una losa corriendo no es un choque: el escalón la sube.
- Volando, correr duplica `fly_speed` como antes, con el mismo estado. Bajar con Shift no lo
  corta, porque un volador no se agacha.

**Salto corriendo.** Un salto con `sprinting` suma `sprint_jump_boost` hacia `facing` (o hacia
el deseo si `facing` es cero).

- Minecraft suma 0,2 bloques por tick, pero en un tick de suelo: deja 0,546 de eso, y el tick
  dura 50 ms. Acá el paso en el suelo dura 15,6 ms, así que 4 bloques por segundo (0,2 por 20)
  daban de más: 8,48 bloques por segundo saltando contra 5,61 corriendo.
- Por defecto es 2,1, calibrado para dar las velocidades de Minecraft en terreno plano: 7,12
  bloques por segundo saltando y corriendo contra 5,61 corriendo (Minecraft: 7,127 y 5,612).

**Agua en bloques con agua (ADR 0025).** `CollisionTable::is_wet` dice si un estado tiene agua:
un fluido o un bloque con agua (`StateFlags::WATERLOGGED`).

- `SwimPlugin` lo usa para saber si el cuerpo está en el agua. Parado sobre una losa con agua,
  los pies están en su celda: está en el agua, como en Minecraft, y camina a la mitad.
- Saltar ahí es un salto. Como en Minecraft, un cuerpo en el suelo con el agua a 0,4 o menos
  sobre los pies (`FLUID_JUMP_THRESHOLD`) salta en vez de nadar hacia arriba. El agua de una celda
  llega a 8/9, o arriba de todo si la celda de arriba también tiene agua; sobre una losa o el
  escalón de abajo de una escalera queda a 0,39. `SwimPlugin` deja el salto al controlador, y
  `SwimIntent::up` no hace nada ahí. Los niveles del agua que corre no se leen: toda celda cuenta
  como fuente.
- `Contacts::in_fluid` (el barrido) y el `in_fluid` de los proyectiles también.
- `is_fluid` sigue siendo solo el bloque fluido: apuntar y poner bloques ven la losa.

**Volar en el agua.** En Minecraft el vuelo creativo no siente los fluidos (`Player` devuelve
`isAffectedByFluids() = !flying`). `SwimPlugin` ahora trata a un volador como seco: no le cambia
los ajustes ni toma su salto como brazada, y su vuelo sigue igual que en el aire. Apagar el vuelo
en el agua le devuelve el agua.

**La cámara** (`examples/shared/player.rs`, solo la vista):

- `PlayerView` en el jugador: la altura del ojo, el campo de visión, la distancia caminada y la
  fuerza del balanceo.
- El ojo va hacia 1,62 o 1,27 según `Crouch`, suave: la diferencia se divide por `1 + 20 dt`
  cada frame (Minecraft la parte a la mitad cada tick, que es la misma tasa) y cae justo en el
  valor al quedar a menos de 1e-4.
- Corriendo, el campo de visión va hacia 1,1 veces el de la cámara, con la misma suavidad.
  Minecraft lo abre 1,15 corriendo y 1,1 volando; acá solo corriendo y 1,1. La cámara recupera el
  suyo cuando el jugador se va (G).
- El balanceo es el `bobView` de Minecraft. Minecraft mueve y gira la escena en el espacio de la
  vista; acá se mueve y gira la cámara, que da lo mismo. Sube hasta `bob` bloques, va
  `bob / 2` a los lados y gira hasta `3 bob` grados sobre su eje y `5 bob` grados hacia abajo.
  `bob` sigue a la velocidad de costado en bloques por tick, hasta 0,1, y es 0 en el aire,
  volando o en el agua. La fase crece 0,6 por bloque caminado.
- La mira apunta con la cámara balanceada: el bloque apuntado es el que está bajo la mira en
  pantalla. En Minecraft la mira se corre un poco del bloque que apunta.
- V apaga y prende el balanceo (`Walking::view_bob`, prendido por defecto). Es la opción de
  video de Minecraft.
- F3 dice `crouching` cuando el cuerpo sigue agachado sin Shift.

## Consecuencias

- Costo, solo en chunks que pueden tener bloques altos (sección 7):
  - Barrido: un movimiento de costado con la base del cuerpo a menos de medio bloque sobre un
    plano entero lee una fila más de bloques. Un caminante sobre el suelo lo hace siempre: una o
    dos lecturas más por capa recorrida. Hacia abajo, la capa donde aterriza se lee entera (de 1
    a 4 bloques para un jugador) en vez de cortar en el primer bloque lleno.
  - Raycast con `Collision`: una lectura más por celda cruzada en su mitad de abajo.
  - En todos lados, cada barrido y cada rayo de colisión miran uno o dos chunks.
  - La guarda de bordes solo corre para quien se agacha en el suelo.
  - El contorno: un draw y unos 100 vértices por frame, armados en CPU.
  - Las mallas no cambian.
- Costo de la sección 8: un personaje agachado sin Shift hace un barrido corto más por paso
  (probar pararse) y no se duerme. Agacharse o pararse inserta un `Collider`, que despierta lo
  que dormía sobre el lugar. El resto son comparaciones por paso. El juego, en el frame: la vista
  hace unas cuentas en CPU.
- Medido con criterion, `tick/1000` (caminantes y cuerpos que caen), base ecd7c8f contra la rama,
  una corrida por lado, tres veces porque la máquina estaba compartida con otras compilaciones:
  657 contra 558 µs, 508 contra 591 y, invirtiendo el orden, 607 contra 524. La segunda corrida
  de cada par sale más lenta, sea cual sea: no hay diferencia que se vea por encima del ruido.
  `tick/1000_fenced`: 564 contra 590 y 519 contra 590, con la rama segunda las dos veces.
- Medido con criterion, una corrida por lado (µs). cf11b4f es antes de las vallas, main es
  5e8177a y la rama ya tiene main mezclado:

  | Bench | cf11b4f | main | rama | rama contra cf11b4f |
  |---|---|---|---|---|
  | `sweep/player_walk` | 0,241 | 0,270 | 0,280 | +16,5 % |
  | `sweep/player_land` | 0,166 | 0,175 | 0,187 | +12,7 % |
  | `sweep/player_fall` | 0,120 | 0,128 | 0,132 | +10,2 % |
  | `sweep/step_wall` | 0,467 | 0,551 | 0,494 | +5,8 % |
  | `sweep/step_wall_slide` | 0,714 | 0,837 | 0,745 | +4,3 % |
  | `raycast/pick_6` | 0,092 | 0,099 | 0,095 | +2,9 % |
  | `raycast/sky_1024` | 7,05 | 12,34 | 7,24 | +2,7 % |
  | `tick/1000` | 466 | 515 | 459 | -1,5 % |
  | `scale/10000_awake` | 1592 | 1720 | 1601 | +0,6 % |
  | `scale/4096_packed` | 7217 | 7671 | 7105 | -1,6 % |
  | `scale/1000_projectiles_one_thread` | 251 | 269 | 258 | +2,7 % |

  - Con una valla en cada chunk, lejos del camino (los benches `_fenced`), la rama contra main:
    barridos de +2,4 a +5,4 %, `sky_1024` +3,1 % y `tick/1000` -2,1 %. Es lo que cuesta mirar
    los chunks donde no ahorra nada.
  - Los barridos sueltos en el bosque (`player_*`) cambian hasta un 10 % entre compilaciones del
    mismo código de físicas: `player_walk` dio 0,255 antes de mezclar main y 0,280 después. Lo
    estable es el ECS (`tick`, `scale`), que es lo que paga el juego.
- Chunks sin cargar: un barrido de costado con la base a menos de medio bloque sobre un plano
  entero, o uno hacia abajo que puede llegar a medio bloque de un techo, necesita también la
  fila de abajo. Un cuerpo en el borde de abajo de un chunk cargado, con el de abajo sin cargar,
  se congela antes que hoy. Igual se congelaba al caer: su piso está en ese chunk.
- Cambios de API:
  - `CharacterController` y `MoveIntent` tienen campos nuevos: un literal sin `..default()`
    deja de compilar.
  - `CollisionBox::is_valid_collision`, `MAX_COLLISION_HEIGHT`, `shapes::fence_outline`.
  - `CollisionTable::{is_tall, reach, tall_states}`, `VoxelSource::tall_free`, `guard_edge`,
    `EDGE_MARGIN`, `FLIGHT_VERTICAL_DRAG`.
  - `boxel_voxel`: `StateSet` y `ChunkData::may_contain`.
  - `RayResult::Unloaded::t` con bloques altos (sección 3).
  - `BlockOutline`, `Outline`, `VoxelRenderer::set_outline`, `outline_edges`,
    `block_outline_edges` y `ShaderModuleId::Outline`.
  - Sección 8: `MoveIntent::facing`, `CharacterController::{sprint_jump_boost, crouch_height,
    sprinting}`, `Crouch`, `Collider::crouched`, `CollisionTable::is_wet`, `Contacts::ran_into`
    y `FLUID_JUMP_THRESHOLD`. `MoveIntent::sprint` ahora empieza el sprint y no lo sostiene.
    `Contacts::in_fluid` y `Swimmer::in_fluid` también cuentan los bloques con agua.
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
  - `boxel_physics/tests/tall_free.rs`: cada barrido, guarda de bordes y rayo se compara bit a
    bit con la versión que lee siempre abajo, en campos al azar con y sin vallas, cruzando bordes
    de chunk y chunks sin cargar, en las cinco bases y en el límite del mundo, con `TestWorld` y
    con un mundo de verdad que se edita. Sin vallas lee menos; con ellas, lo mismo. También
    rayos apuntados al borde de una losa sobre el plano de salida de su celda, donde la caja
    queda un ulp después de la salida. En `ray.rs`, los puntos donde el rayo mira otro chunk se
    comparan con avanzar de a un bloque.
  - `boxel_voxel/tests/contains.rs`: `may_contain` nunca dice que no de más y su respuesta
    guardada sigue a `set`, `blocks_mut` y `compact`.
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
    Sección 8: agachado (despertando de dormido) mide 1,5 con los pies en su lugar y al soltar
    vuelve el mismo `Collider`; de pie un techo de losas a 1,5 lo frena y agachado pasa abajo;
    ahí, sin Shift, sigue agachado, lento y despierto, y se para apenas sacan el techo; afuera se
    para y camina a toda velocidad; un volador con Shift no se agacha. Correr no empieza de
    costado ni para atrás, empieza hacia adelante (también en diagonal) y sigue sin la tecla;
    para atrás, al parar, al agacharse y contra una pared termina. Rozando una pared a 5 grados
    sigue y a 15 termina; con la tecla apretada, a 5 grados corre a lo largo de la pared y salta
    con empujón, y a 15 camina y salta sin él. Saltando y corriendo da 7,12 bloques por segundo
    contra 5,61. El recorrido bit a bit en cinco bases suma correr, saltar contra una pared y
    quedar agachado bajo un techo.
  - `boxel_physics/tests/swim.rs`: parado sobre losas con agua está en el agua, sobre losas secas
    no; ahí nadar hacia arriba no lo mueve y saltar llega tan alto como en la losa seca (más de un
    bloque sobre la losa), igual bit a bit lejos del origen. Un volador en el agua no se hunde,
    sube como en el aire, queda quieto exacto con sus ajustes de tierra, y al apagar el vuelo el
    agua lo toma; igual bit a bit en tres bases. `block_shapes.rs`: el agua y la losa con agua
    tienen agua, la losa con agua no es fluido y choca como la seca.
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
    Sección 8: con Shift el ojo baja suave hasta 1,27 y vuelve justo a 1,62; con W y Ctrl el
    campo de visión se abre suave hasta 1,1, sigue sin Ctrl y vuelve justo al soltar W, y con G
    la cámara recupera el suyo; caminando la vista se balancea (hasta una décima de bloque), con
    V no, y volando tampoco. En `shared/player.rs`: la suavidad cae justo y no se pasa; el
    balanceo queda en sus límites y se repite cada 2 de distancia; la vista se agacha, se abre y
    se balancea, y un salto grande no cuenta como caminar.

## Fuera de esta parte

- Muros de piedra y puertas de valla (también chocan a 1,5).
- De la issue #37 queda la interacción con bloques (abrir puertas y similares, cuando existan) y
  el hambre (correr no la pide).
- Nadar como en Minecraft más allá de subir: correr bajo el agua (la pose de nado de 0,6), que
  Shift baje en el agua, y que correr se corte en la superficie del agua.
- La brazada de Minecraft es un empujón chico por tick; la nuestra pone 3 bloques por segundo de
  golpe. Con Espacio mantenido sobre losas con agua, el primer salto sale entero, pero al caer el
  cuerpo nada en el agua de la losa y flota hasta soltar Espacio. En Minecraft vuelve a saltar.
- Correr con dos toques de W, y el modo "alternar" de la tecla de correr.
- El campo de visión al volar (Minecraft lo abre 1,1) y el giro de la cámara al recibir daño.

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
- Achicar la caja del agachado solo dentro del paso, sin cambiar el `Collider`: los choques
  entre cuerpos, el nado y las consultas seguirían viendo 1,8.
- Un sistema aparte, antes del paso, para la pose: no tiene los obstáculos del paso (se arman
  después del controlador) y el `Collider` nuevo llegaría un paso tarde.
- El impulso del salto corriendo de 4 bloques por segundo (0,2 por tick sin más): con pasos de
  15,6 ms en el suelo pierde menos que en el tick de 50 ms de Minecraft y da 8,5 bloques por
  segundo en vez de 7,1.
