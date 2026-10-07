# ADR 0021: fluidos (agua que fluye, lagos y mar)

- Estado: aceptado (tercera parte de la issue #38: líquidos que fluyen)
- Fecha: 2026-10-07
- Revisa: ADR 0018 sección 6 (la superficie del agua) y ADR 0019 (ids del contenido de serie)

## Contexto

Con el ADR 0018 el agua se ve, pero es un cubo quieto con el techo a 14/16. Para un clon de
Minecraft falta lo principal: que el agua corra. Eso pide cuatro cosas que el motor no tenía:

- Un estado por nivel de agua (el ADR 0019 ya deja `level` 0 a 15).
- Bloques que se actualizan solos más tarde (los "scheduled ticks" de Minecraft), deterministas,
  sin tirones aunque se inunde media cueva, y que esperen en chunks no cargados.
- Una superficie con altura por esquina, para que una pendiente se vea continua. Hoy el quad de
  superficie es greedy y baja una constante.
- Agua en el mundo generado: lagos y mar.

## Decisión

### 1. Estados

- `boxel:water` tiene `level` 0 a 15 (`FLUID_LEVEL`), como Minecraft: 0 es fuente, 1 a 7 agua
  que corre (más baja cuanto mayor el nivel), 8 a 15 agua que cae (alimentada desde arriba).
- Se registra con `add_block_with_states`. Todos los niveles se dibujan, chocan y dejan pasar la
  luz igual (opacidad 1, el cielo se corta). Solo los ticks y la malla leen el nivel.
- Los ids después del agua se corren 15: tablones 28, lámparas 29 a 32, lava 33, vidrio 34, losa
  35 a 37, plantas 38 y 39. Los ids no se guardan, así que solo cambian tests.

### 2. Ticks programados (`boxel_world::ticks`)

Un mecanismo del motor, separado del agua:

- **Reloj:** 20 ticks de juego por segundo (`GAME_TICKS_PER_SECOND`), contados desde los pasos
  fijos (64 Hz, en `FixedUpdate`) con aritmética entera. Igual a cualquier frame rate.
- **Cola:** `BlockTicks` guarda `(tick de vencimiento, orden) -> posición` en una sola cola
  ordenada. Los ticks de un mismo tick de juego corren en el orden en que se pidieron, sin
  importar el chunk: el resultado es determinista. Una posición se programa una sola vez (un
  pedido mientras otro espera se descarta, como en Minecraft).
- **Manejadores:** `TickHandler { delay, reach, tick }` por bloque (`BlockTicks::register`).
- **Cuándo corre:** una vez por frame, entre `WorldSystems::Interest` y `WorldSystems::Edits`,
  con un presupuesto de tiempo (`BlockTickSettings`, 1 ms y al menos 64 ticks por frame). Una
  inundación grande se reparte en varios frames. Un tick atrasado programa lo siguiente relativo
  a su propio vencimiento, así que el presupuesto solo atrasa, no cambia el resultado.
- **Escrituras:** van por el `EditQueue`, un lote por frame. Luz, remallado y `BlockChanged`
  funcionan como con cualquier edición. Dentro del frame, los ticks siguientes leen lo que
  escribieron los anteriores desde una capa encima del mapa, como el `setBlock` de Minecraft.
- **Vecinos:** cada cambio avisa al bloque y a sus 6 vecinos; los que tienen manejador se
  programan `delay` ticks después. Las escrituras de un tick avisan en su tick; las demás
  ediciones (jugador, herramientas) en el tick actual, desde los `BlockChanged` del frame (el eco
  de las escrituras propias se descarta).
- **Chunks no cargados:** un tick corre solo si todos los chunks a su alcance (`reach`) están
  cargados. Si no, se congela esperando ese chunk y corre apenas llega su
  `ChunkDataEvent::Loaded`.
- El agua generada no programa nada: un lago queda quieto hasta que una edición lo toca.

### 3. Reglas del agua (`boxel_world::fluid`, `FluidPlugin`)

Las de `FlowingFluid` de Minecraft Java:

- Un tick de agua que no es fuente se recalcula desde sus vecinos: cae si hay agua arriba; es
  fuente si dos vecinos horizontales son fuentes y abajo hay algo sólido o una fuente (agua
  infinita); si no, el mayor nivel vecino menos uno, o aire si llega a cero (así se seca sin su
  fuente).
- Después se reparte: hacia abajo si la celda de abajo puede tenerla (como agua que cae), y a los
  costados solo si no puede caer. A los costados elige las direcciones con el camino más corto a
  un desnivel dentro de 4 bloques, o todas si no hay. Llega a 7 bloques.
- Una celda puede tener agua si es aire o algo sin colisión que no es otro fluido (las plantas,
  que el agua se lleva).
- Demora 5 ticks (4 pasos por segundo). Alcance de lectura: 5 bloques a los lados, 2 abajo y 1
  arriba.

### 4. Malla: altura por esquina

- Todos los niveles de un fluido se mallan con el material de su fuente: se tapan y se fusionan
  como un solo fluido. Los niveles de las celdas que no son fuente van a
  `PaddedChunk::fluid_levels`.
- Cada celda de superficie calcula la altura de sus 4 esquinas como Minecraft: media pesada de
  las 4 celdas alrededor de la esquina (peso 10 para fuentes y celdas llenas, 1 para el resto,
  las sólidas no cuentan; la diagonal cuenta solo si un costado tiene agua; un costado lleno hace
  la esquina llena). La fuente está a 14/16 (nuestra superficie de siempre) y el nivel `l` a
  `(8 - l) / 8` de eso. Se redondea a dieciseisavos.
- **Una celda con las 4 esquinas a 14/16 sigue en el greedy** (`SHAPE_FLUID_SURFACE`): un lago
  quieto cuesta los mismos quads que antes.
- Las demás salen de las máscaras del greedy (sus vecinos siguen tapando caras contra ellas) y se
  dibujan por bloque con el código de forma 7, `SHAPE_FLUID_SLOPE`. Como esos quads tienen
  `w = h = 1`, los 10 bits de `w - 1` y `h - 1` y el byte de AO guardan las cuatro alturas (4
  bits cada una). Estos quads no tienen AO, como el agua de Minecraft.
- El shader arma el techo por las 4 esquinas y los costados desde 0 hasta las esquinas de su
  borde. Es el mismo código en los tres tiers.
- Sin grietas: dos celdas de agua que comparten una esquina ven las mismas 4 celdas y cuentan las
  mismas, también entre chunks (el borde del chunk trae esas celdas y la de arriba de cada una).
- Se va la ranura de 2/16 entre un agua bajo un bloque y una de superficie (ADR 0018 sección 6):
  la celda llena sube las esquinas vecinas.
- Orden translúcido: el techo inclinado no tiene plano. El ordenador lo trata como una franja
  horizontal entre su esquina más baja y la más alta, que lo ordena contra lo que queda entero
  arriba o abajo. Los bordes de la franja limitan la celda de la cámara como planos.
- Textura: solo la quieta. Falta la textura de flujo (`water_flow`) rotada según la corriente.

### 5. Mundo generado: mar y playas (`boxel_worldgen::Sea`)

- Las columnas por debajo del nivel del mar (46) se llenan de agua fuente hasta ese nivel. Las
  columnas hasta 2 bloques por encima (playas) y las de abajo llevan arena arriba; ahí no crecen
  árboles ni plantas. Cerca del 8 % de las columnas del terreno por defecto quedan bajo agua.
- Es una corrida más en el relleno de cada columna, a partir de su altura sola: sin datos de
  vecinos, exacto a cualquier distancia y casi gratis.
- En el overworld los 4 bloques de suelo bajo el agua nunca se excavan (`Sea::FLOOR`): el mar no
  queda sobre una cueva. Una cueva sí puede abrirse de costado junto al mar: ahí el agua queda
  quieta como una pared hasta que algo la toca.
- El forest y el overworld tienen mar por defecto. `TerrainGenerator` no cambia (sus goldens).
  `--no-sea` en los ejemplos los deja secos.

### 6. Física: nadar (`SwimPlugin`, opcional)

- Un cuerpo con `Swimmer` cuya caja toca agua: un cuarto de la gravedad, arrastre 3 /s en todas
  las direcciones (se hunde a unos 2.7 bloques/s), la mitad de la velocidad al caminar, y sube a
  3 bloques/s con `SwimIntent::up` o al apretar salto (no salta desde el fondo).
- Es un módulo nuevo y un plugin aparte: antes de los controladores de cada paso fijo cambia los
  `BodySettings` y la velocidad de caminar de tierra por los de agua, y al salir los devuelve. No
  toca el paso del cuerpo ni el del controlador.

### 7. Ejemplos

- `voxel_world`: el agua puesta es una fuente que corre; romper un bloque junto a un lago lo hace
  correr; F3 muestra la primera agua en la mira con su nivel (`boxel:water[level=3]`) y una línea
  de los ticks. El jugador nada.
- `snapshot --flow N` deja correr N ticks después de las ediciones, uno a uno con el reloj
  quieto: las mismas imágenes en cada corrida. Por ejemplo:
  - lago: `--world forest --camera 205,66,-205,-40,-20`;
  - cascada: `--fill stone@228,30,-232:232,56,-228 --place water@230,57,-230 --flow 300`;
  - pendiente: `--fill stone@138,58,-142:162,62,-118 --fill air@138,63,-142:162,80,-118
    --place water@150,63,-130 --flow 120`.

## Consecuencias

- Los tests corren el motor contra un simulador de fuerza bruta escrito aparte (sin chunks, sin
  presupuesto, sin capa), tick a tick: distancia 7, el camino hacia abajo, la caída, la fuente
  nueva, el secado, terreno al azar con ediciones, lejos del origen (±2^31, 2^40 y el límite del
  mundo). Además el presupuesto (solo atrasa), los ticks congelados y el agua generada quieta.
- La malla se prueba contra un oráculo de alturas por esquina, sin grietas entre chunks, y los
  tres tiers dibujan agua inclinada igual píxel a píxel.
- Rendimiento: ver la tabla de mediciones abajo.
- Un `BodySettings` cambiado por el juego mientras el cuerpo está en el agua se pierde al salir.
- Los ticks congelados de un chunk que nunca vuelve quedan en memoria. Hoy los chunks no se
  guardan, así que un chunk que vuelve se genera de nuevo y el tick corre sobre él (si ya no hay
  agua, no hace nada).
- En un mundo con límites verticales, un tick que lee fuera de ellos espera para siempre.

## Mediciones

PENDIENTE

## Fuera de esta parte

- La textura de flujo y su animación; el agua animada en general.
- La lava que corre (la lava sigue siendo un cubo opaco que brilla).
- `waterlogged` (bloques con agua adentro) y las interacciones agua y lava.
- Guardar los ticks con los chunks (llega con la persistencia, M1).

## Alternativas descartadas

- Una tabla de alturas por chunk en el arena: necesita un atributo más en Low (un solo bloque de
  uniforms, una sola textura) y otro rango en High y Mid. Los bits libres de los quads por bloque
  alcanzan.
- Todas las celdas de agua por el camino por bloque: un lago costaría un quad por celda.
- Una cola de ticks por chunk: el orden entre chunks dejaría de ser total y el resultado
  dependería del orden en que se recorren. La cola global ordena todo; los ticks congelados sí
  van por chunk.
- Leer el mapa directamente sin capa: los ticks de un mismo frame no verían lo que escribieron
  los anteriores hasta el frame siguiente, y el resultado cambiaría con el presupuesto.
