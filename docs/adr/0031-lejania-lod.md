# ADR 0031: lejanía con LOD, un plugin opcional

- Estado: borrador. El M1, el M3 y parte del M5 (en las rondas de optimización) y el M2
  (ediciones y respaldo) están hechos en la rama `feat/lod` (sobre `feat/fog`), con los números
  finales medidos sobre 3506990. Falta la prueba del dueño ("Qué mirar en el vuelo del dueño",
  al final).
- Fecha: 2026-10-11

## Contexto

El dueño pidió terreno mucho más allá de la distancia de vista, como el mod Distant Horizons de
Minecraft: montañas, bosques y mares hasta el horizonte, simplificados. Las condiciones:

- Es un plugin opcional, aparte, como la niebla (ADR 0030). Sin él no hay costo.
- La Radeon 780M no tiene que moverlo a 144 fps. El motor sí, pero esto es opcional.

La issue #30 ("M3 escala") preguntaba cómo hacer el LOD: con mips de los chunks de 32³ o de otra
forma. El plan completo está en `.claude/plans/lod-plan.md`. El dueño tomó sus decisiones con las
recomendaciones del plan:

1. Se permiten los ganchos de render y shaders de este ADR: un módulo nuevo `lod.wgsl` y una
   `LodPass`; ningún shader existente cambia.
2. El generador muestrea cada nivel directamente. Los mips desde chunks reales quedan solo para
   ediciones y para generadores sin fuente de LOD (M2).
3. Tres presets: `near`, `far` (el de `--lod` por defecto) y `extreme`. Sin detección del
   dispositivo: el preset lo elige el juego.
4. La regla de la celda: gana cualquier bloque sólido, salvo agua sobre el suelo.
5. Agua lejana opaca primero.
6. Las capas de niebla mantienen su alcance en el M1.
7. LOD apagado en backends GL y en adaptadores de software.
8. Nada del LOD generado se guarda en disco.
9. La rama sale de `feat/fog`.
10. El vacío bajo el piso del mundo cuenta como sólido.

Restricciones del motor que pesan aquí:

- Las imágenes deben salir iguales en High, Mid y Low, y el tier Low liga un solo uniform block
  (ADR 0011).
- Coordenadas `i64`, exactas lejos del origen (ADR 0014).
- No se tocan el streaming (`streaming.rs`, `interest.rs`), el scheduler, la visibilidad, el ritmo
  de frames ni `boxel_gfx`.

## Decisión

### 1. Un plugin aparte, sin costo cuando no está

`LodPlugin { settings, executor }` (`boxel_render::lod`, en el prelude con `LodSettings`) no está
en `DefaultPlugins`. Va después de `WorldPlugin` y de `RenderPlugin`. Al agregarlo:

- agrega `LodWorldPlugin` (`boxel_world::lod`), el lado del mundo: los recursos (`LodSettings`,
  `LodStats`, `LodOps`, `LodFeedback`, `LodCoverage`), el mensaje `LodResync` y la cadena
  `LodSystems` en `Update`, después de la visibilidad del mundo;
- agrega el sistema de foco: la cámara activa pasa a `LodFocus` antes del plan;
- agrega `extract_lod` en `RenderSystems::Extract`, después del `extract` del render y antes de la
  búsqueda de cuevas: las ops del frame pasan a `ExtractedFrame.lod`.

Sin el plugin no existe nada del LOD: ni recursos, ni sistemas, ni jobs, ni pipeline, ni módulo
de shader. El uniform `View` y la vista de recorte son los de antes. Un test de GPU lo comprueba
con imágenes idénticas byte a byte.

Las apps sin ECS (tests, `snapshot`) usan `VoxelRenderer::set_lod(device, Some(LodInput))`, y
`headless_world` usa solo `LodWorldPlugin`.

Con el plugin, el smoke test de la GPU también construye la pasada y dibuja un quad de nivel 2.
Si falla, el LOD queda apagado en ese dispositivo con un aviso. Nunca cambia el tier.

Los ajustes se cambian en tiempo de ejecución con `ResMut<LodSettings>`. Un cambio inválido se
avisa una vez y quedan los últimos válidos, como en la niebla.

### 2. Nodos, niveles y presets

Un nodo de nivel `k` (de 1 a K) es una grilla de 32³ celdas; cada celda cubre `2^k` bloques por
lado, así que el nodo mide `32 * 2^k` bloques. La clave es la `ChunkKey` de siempre con `lod = k`.
Los nodos son 3D: Y no tiene límite y una cámara alta ve el suelo con un nivel más grueso.

La selección es un octree refinado por distancia 3D, con el mismo radio `R` (en nodos de su nivel)
en cada nivel: anillos. Con `R = 24` una celda en el borde interior de cada anillo mide unos 2
píxeles a 1080p.

| preset | R | K | horizonte | arena |
|---|---:|---:|---:|---:|
| `near` | 16 | 3 | 4.096 bloques | 4 Mi quads |
| `far` | 24 | 4 | 12.288 bloques | 12 Mi quads |
| `extreme` | 24 | 6 | 49.152 bloques | 16 Mi quads |

Los presets viven en `examples/shared/lod.rs`: son distancias y elecciones del dispositivo, no
contenido. `boxel_demo` no puede usar `LodSettings` (el mundo y el render lo usan en sus tests,
como en ADR 0030).

### 3. De dónde salen las celdas: el generador muestrea cada nivel

`WorldGenerator` gana un método con valor por defecto, `lod() -> Option<&dyn LodSource>`
(`None`). La fuente (`boxel_worldgen::lod`) llena una columna de nodos de un nivel por llamada:

- `LodRequest`: dimensión, nivel, columna y capas pedidas;
- emite cada capa como `Uniform(estado)` o `Cells` (34³ celdas: los 32³ del nodo y un borde de
  una celda);
- devuelve `LodColumn { lo, hi, below }`: debajo de `lo` todo es `below`, desde `hi` todo es aire,
  y cada capa fuera de `[lo, hi)` es uniforme en todo su bloque acolchado, bordes incluidos. Así
  una capa uniforme nunca necesita una cara.

Un nodo nunca espera a sus vecinos ni a los chunks reales. Esto responde la pregunta de la issue
#30: armar el nivel 4 desde chunks reales cuesta unos 90 ms por columna; el generador lo hace en
unos 0,3 ms.

`boxel_demo` implementa la fuente para `ForestGenerator`, `TerrainGenerator` y
`OverworldGenerator`, que devuelven `Some(self)`. Cada llamada lee los campos actuales del
generador (el ejemplo cambia el mar después de `new()`). Por columna:

- alturas en las esquinas de las celdas, una llamada a `heights_with`;
- el suelo de una celda es el máximo de sus esquinas más un margen (sección "Desvíos");
- árboles con `tree_in_cell`, rasterizados con la regla de la celda; en los niveles 5 y 6 se mira
  uno de cada 4 y de cada 16 lugares de árbol;
- mar con su cláusula de agua sobre suelo, y playas de arena;
- las cuevas y las plantas se ignoran; bajo el piso del mundo todo es piedra.

`FlatGenerator` tiene una fuente exacta: sus capas son planas, así que cada celda mira solo sus
filas de bloques. Es el oráculo de los tests del motor.

### 4. La regla de la celda

Es el contrato de toda fuente (`boxel_voxel::lod`, documentado en `boxel_worldgen::lod`). Una
celda de nivel `k` abarca `s = 2^k` bloques por lado:

1. Si algún bloque es sólido: si el fluido más alto queda arriba del sólido más alto y hay al
   menos `s²/2` bloques de fluido, la celda es ese fluido. Si no, es sólida, con el estado del
   sólido más alto.
2. Si no hay sólido y al menos la mitad es fluido, es fluido.
3. Si no, aire.

La clase de cada estado sale de `RenderShape` y `BlockModel`, nunca de nombres: las plantas y el
vidrio son vacíos; las hojas y los cubos cutout, sólidos; una losa o una caja que llena al menos
media celda, sólida. Así las torres, los troncos y las copas sobreviven en todos los niveles, y
los mares poco profundos siguen siendo agua lejos (si no, a 3 km todo mar de menos de 6 bloques
sería arena).

### 5. Mallado

El mallador de siempre (`Mesher::mesh`, sin cambios) sobre un `PaddedChunk` llenado a mano, con
una tabla de materiales del LOD: todo lo visible es un cubo opaco, el agua también, y estados
parecidos se fusionan (`LodMaterials`). Sin luz y sin AO. Después, en `boxel_mesh::lod`:

- **Octantes:** los quads se cortan en la celda 16, así cada quad queda en un octante del nodo.
- **Faldones:** en las 6 caras del nodo y en los 3 planos internos, una cara hacia el plano donde
  un sólido o un fluido toca otro y hay aire cerca. Tapan las grietas entre niveles y con el
  detalle completo.
- **Paredes de agua (solo nivel 1):** caras en esos planos cerca del agua, para que un rayo que
  pasa bajo el agua translúcida cercana no vea el cielo.

La salida es el `PackedQuad` de siempre (8 bytes), en celdas.

### 6. Qué nodos se quieren

Umbrales en bloques (`e_k` es el lado de un nodo de nivel `k`):

- `T_1` cubre toda la región que el renderer principal todavía puede dibujar: la región de
  conservación 3D de los viewers (radio, radio vertical y la histéresis de descarga, recortada a
  `vertical_bounds`) más un nodo. Nunca baja de `64 R`.
- Si la región se achica (un radio menor), los chunks que quedan afuera siguen publicados hasta
  que el mundo los descarga, de a poco. `T_1` conserva su alcance hasta que todos esos chunks
  vuelven a caer en nodos de nivel 1 queridos; si no, un nodo de nivel 2 ocultaría ese octante
  sin que nadie arme el hijo.
- `T_k = max(32 R 2^k, T_(k-1) + 2 e_k)`, y el horizonte `H = T_K`.
- Si `T_1` pasa el horizonte del preset (un radio vertical enorme sin límites), el LOD avisa una
  vez y no dibuja nada (`LodStats.off`). Vuelve recién cuando `T_1` queda 128 bloques debajo, así
  un ojo que sube y baja sobre la línea no vacía y rellena el LOD cada vez.
- Ninguna capa por encima de la última capa de chunks del generador
  (`vertical_chunks().end() >> k`) se quiere: ahí todo es aire. Sin esto, un ojo alto sobre un
  mundo de altura fija pedía un disco de aire tan ancho como `T_1` a su propia altura (a y 3.000,
  casi el doble de columnas que en el suelo). La excepción son las ediciones (M2): una columna
  con chunks editados más arriba (una torre o una plataforma sobre el terreno) quiere hasta la
  capa del más alto, en cada nivel (`plan::Ceiling`).
- Los viewers a más del horizonte del preset no cuentan para la región de conservación: después
  de un salto, el registro del viewer en el mundo llega un frame tarde y encendía la guarda.

Un nodo de nivel `k` se refina mientras su caja está a menos de `T_(k-1)` del ojo, con una
histéresis de un nodo hijo. Cada nivel es una ventana densa de columnas, y un nivel se vuelve a
planear cuando el ojo se movió un octavo de su banda (sección "Desvíos"). Un replaneo va del nivel
más grueso al más fino, unas filas por frame dentro del presupuesto.

### 7. Cobertura: el LOD nunca dibuja donde dibuja el detalle completo

Cada nodo lleva una máscara de 8 bits de octantes ocultos; el vertex shader colapsa los quads de
esos octantes. El LOD y el detalle completo nunca dibujan el mismo volumen, así que no hay bias de
profundidad ni z-fighting.

- Un chunk real cuenta como cubierto si su último `ChunkMeshEvent` fue un `Upsert` y el renderer
  principal no lo rechazó, o si su malla quedó vacía, o si es todo aire. Nunca se usa
  `ChunkSlot::shown()`: el LOD taparía las construcciones del jugador.
- Los mensajes se leen completos cada frame, sin presupuesto, así que la máscara cambia en el
  mismo frame que el chunk: ni hueco ni doble.
- En el nivel 1 un octante es exactamente un chunk.
- En el nivel 2 y más, un octante se oculta si su hijo está cubierto, o si su hijo tiene chunks
  publicados y todavía no está listo (un hueco corto antes que una superposición). Un hijo
  perdido (falló, o fue rechazado y espera) no oculta: su padre dibuja. En una columna cuyo
  trabajo falló, las capas que vuelven a entrar quedan perdidas enseguida, no esperando un
  trabajo que nunca corre.
- **La regla de la cadena:** un nodo se envía solo si todos sus ancestros están en el renderer, y
  un nodo rechazado retira a sus descendientes.
- Los nodos de nivel 1 bajo el detalle completo siguen armados: tapan el borde mientras cargan
  los chunks al volar.
- Sin renderer (todavía no hay, o se perdió el dispositivo) `extract_lod` descarta las ops. Todo
  renderer nuevo empieza con un frame de resync; en ese frame, y cuando cambia la arena, el
  renderer tira sus nodos y `extract_lod` escribe `LodResync`: todo nodo deja de estar
  disponible y se vuelve a enviar, del nivel más grueso al más fino.
- Los rechazos del renderer principal llegan por `LodFeedback.chunks`, pero solo los que dejan el
  chunk sin dibujar (`take_undrawn`): un chunk residente que no entra en la arena llena conserva
  su malla vieja y sigue dibujado. Un `Upsert` cuenta como dibujado desde su frame, como un chunk
  nuevo; un rechazo leído el frame después de un `Upsert` es de la subida anterior y no cambia
  nada.

### 8. Trabajos y presupuesto

- Un trabajo es una columna de un nivel: la fuente, y para cada nodo con celdas el llenado, el
  mallado y las pasadas (a lo sumo 3 nodos). Meta: 0,3 ms.
- Corren en el pool de cómputo asíncrono con un ejecutor propio (el del mundo es privado).
- A lo sumo 2 en vuelo mientras el mundo carga, y solo de clase 0 (el horizonte y los huecos);
  5 cuando se asentó. Un chunk cercano espera a lo sumo un trabajo del LOD por hilo.
- Los hilos no esperan al principal: por cada lugar en vuelo hay 4 trabajos ya armados, y un
  hilo que termina uno empieza el siguiente. El respaldo no arma trabajos de antemano.
- Prioridades: clase 0 el nivel más grueso (el horizonte aparece primero) y los nodos que cierran
  un hueco (un nodo de nivel 1 con chunks publicados que el detalle completo no cubre entero, y
  sus ancestros); clase 1 lo que se ve; clase 2 el resto; clase 3 los nodos de nivel 1 bajo el
  detalle completo que no cierran ningún hueco.
- Un solo presupuesto por frame en el hilo principal (`frame_budget`, 0,3 ms), en este orden:
  el replaneo, juntar y lanzar trabajos y leer los eventos de datos del mundo. Mientras el mundo
  carga es un tercio, porque el mundo va primero. Con el mundo asentado los trabajos tienen su
  propio presupuesto. Los eventos de mallas y los mensajes del renderer se leen completos, fuera
  del presupuesto: de ellos depende qué dibuja el detalle completo.
- Envía a lo sumo 192 nodos y 2 MiB por frame. Entre frames (ADR 0023) un paso `Idle` sigue
  juntando y lanzando, sin escribir ops. Con el respaldo solo junta: sus trabajos copian los
  chunks cargados al crearse, y eso pasa en `Update`.

### 9. Cómo se dibuja

- `LodPass` dentro de `VoxelRenderer`: su propia arena (`VERTEX | COPY_DST`, también `STORAGE`
  en High, sub-asignada como la principal), así el LOD nunca le quita lugar al detalle completo.
  Las subidas van en un buffer de staging por frame, a lo sumo 4 frames de envíos del mundo por
  frame: después de frames sin dibujar (ventana oculta) lo atrasado sube en varios frames, sin un
  tirón. Un nodo se dibuja solo con sus quads arriba; si no se puede crear el staging (sin
  memoria), esos nodos esperan al frame siguiente en vez de dibujar basura.
- Dos caminos con las mismas funciones de vértice y fragmento (`lod_common.wgsl`), así que los
  mismos píxeles:
  - Mid, Low y las GPU integradas (también en High): recorte en CPU (frustum y prueba de caras
    por caja) y una llamada instanciada por tramo de baldes de caras que pueden mirar a la cámara
    (`lod.wgsl`): el quad instanciado del Low, un uniform block (`View`), una textura (la mip de
    1x1 de cada capa, el color medio), 16 bytes de immediates.
  - High en GPU dedicadas (el M3): los nodos viven en una tabla en la GPU (`LodNodeGpu`, 64 B;
    solo se escriben las entradas que cambian). `lod_cull.wgsl` deja los baldes que pueden verse
    y los escribe en listas indirectas, una por segmento de 10.922 lugares, cada una un
    `multi_draw_indexed_indirect_count` con vertex pulling (`lod_pull.wgsl`). La geometría del
    LOD va en el grupo 3. En la 780M este camino era más lento, por eso las integradas usan el
    otro.
- **Profundidad jerárquica (el M5, adelantado, solo High).** La pasada principal se corta después
  de lo opaco y cutout, y el recorte del LOD descarta los nodos tapados en todo su rectángulo por
  una pirámide de esa profundidad (`lod_hiz.wgsl`). El recorte cuenta cuántos tapa (una lectura
  asíncrona que nunca se espera): mientras tapa a lo sumo 1 de cada 16, los 30 frames siguientes
  recortan sin pirámide y sin cortar la pasada. Los píxeles son los mismos.
- **Bajo tierra** no se dibuja nada en ningún tier: si la búsqueda de cuevas no llega al borde de
  su caja, ninguna línea de vista sale de ella.
- **Faldones solo donde hay costura.** Cada quad del LOD lleva su costura en el índice de la
  paleta de luz (una paleta de 3 entradas de cielo abierto), y los de costura van al final de
  cada balde. Los faldones y las paredes de agua se dibujan junto a un octante oculto o en una
  cara del nodo cuyo vecino del mismo nivel no dibuja ahí; en el resto quedan entre celdas
  sólidas. Los dos caminos cortan igual (`bucket_quads`).
- Se dibuja en la pasada principal justo después de lo opaco y cutout, antes del agua
  translúcida y de la niebla. En High los draws translúcidos ligan su grupo 2 y su buffer de
  índices, así la pasada puede seguir partida.
- Luz de cielo abierto (`FULL_SKY_LIGHT`): el ciclo de día y noche funciona; no hay luz de
  bloque lejos.
- Con el LOD dibujando, el renderer mueve el plano lejano del recorte de chunks al horizonte (las
  vistas de recorte de Mid y Low, el frustum del cull de High y el orden de los translúcidos), y
  la búsqueda de cuevas también (`frame_visibility_with`, con el mismo `lod_far` que usa
  `Prepare`). Los chunks publicados llegan más allá de la distancia de vista (la banda de
  conservación y las esquinas de la caja). Si no se dibujaran, quedaría un hueco donde el LOD se
  hizo a un lado.
- La niebla de distancia del terreno usa la misma regla que sin LOD, medida sobre `H` en lugar
  de la distancia de vista (`RenderSettings::distance_fog`, issue #51): solo el último 10 %, como
  mucho 64 bloques. Con `far` va de 12.224 a 12.288 bloques. El detalle completo y el LOD se
  funden juntos con el cielo. Con la niebla apagada el borde queda nítido, con o sin LOD.
  `ViewParams` no cambia, así que las capas de `FogPlugin` conservan su alcance. Antes iba de la
  mitad de `H` a `H` (`LodSettings::fade_start`, ya no existe).
- En GL el LOD queda apagado (sin clip control la profundidad lejana pelea). `voxel_world` lo
  apaga en adaptadores de software, sea cual sea el radio. Si el renderer no puede dibujar el LOD
  (GL, el smoke falló o la pasada no se pudo crear), `extract_lod` lo avisa en
  `LodFeedback::cannot_draw` y el lado del mundo también se apaga (`LodOff::Unsupported`): ni
  trabajos ni mallas para nada.
- F9 tiñe cada nivel con un color fijo (rojo, verde, azul, amarillo, magenta, cian).

### 10. Ejemplos y herramientas

- `voxel_world --lod near|far|extreme`: F8 recorre far, extreme, oculto y near; F9 tiñe los
  niveles; F3 muestra una línea `lod` (horizonte, nodos, dibujados, draws, quads, trabajos,
  llenado y rechazos). F10 prende y apaga la niebla de distancia, con o sin LOD.
- `--no-distance-fog` (en `voxel_world`, `snapshot` y `stress`) arranca sin niebla de distancia.
- `stress --lod PRESET`: el JSON trae `run.lod` y `run.lod_fill_s` (cuándo terminó el primer
  llenado), para separar los frames del llenado.
- `snapshot --lod PRESET`: espera el llenado y lo dibuja; imprime una línea `lod:`.
  `--lod-tint` tiñe los niveles y `--fixed-height` carga como en `voxel_world`.
- `snapshot` con `--fill` o `--world-dir` muestra las ediciones lejos; la línea `lod:` dice
  cuántos chunks editados dibuja el LOD (`edited_chunks`).
- `stress --screenshot PATH [--screenshot-yaw GRADOS]`: la captura del frame donde para el vuelo,
  girada si se pide, para mirar el borde que avanza y la banda que queda atrás.
- `headless_world --lod PRESET`: nodos y quads por nivel, nodos del piso aparte, tiempos de los
  trabajos y del llenado, sin GPU.
- `tools/bench_report.py` cuenta `lod` entre los ajustes; las corridas viejas valen `off`.

### 11. Ediciones a distancia y respaldo (M2)

El generador no sabe lo que construyó el jugador. El LOD guarda el conjunto de chunks editados
(`boxel_world::lod::edits`):

- todo chunk que nombra un `BlockChanged` y, con `PersistPlugin`, todo chunk guardado (se listan
  una vez al arrancar, con un accesor nuevo en `persist/saver.rs`);
- de cada uno, una **pirámide**: sus celdas en los niveles 1 a 5 con la regla exacta, leídas de
  sus bloques (el chunk cargado, si no el guardado, si no el del generador). En esos niveles una
  celda cae en un solo chunk. Unos 9,4 KiB por chunk, en una caché de 2.048 (la menos usada sale);
- un trabajo de columna pone las pirámides de los chunks editados que toca sobre las celdas de la
  fuente, bordes incluidos, y agranda la banda de la columna hasta sus capas: una torre más alta
  que el terreno tiene su nodo.

Un chunk que cambia espera `edit_debounce` (500 ms) desde su último cambio; después un trabajo de
pirámide lee sus bloques y los nodos cuyas celdas acolchadas llegan a él quedan **viejos**. Se
rearman con su prioridad de siempre. Mientras tanto el renderer sigue dibujando lo que tenía de
ellos (su malla vieja, o nada si estaba vacío) y la malla nueva lo reemplaza en las ops del mismo
frame: una edición nunca abre un hueco.

**Los vecinos cambian juntos.** Una edición en el borde de un nodo deja viejos a los dos nodos
del mismo nivel que la tocan, y cada uno se rearma en su propio trabajo. Si uno cambiara antes,
el render dejaría afuera los faldones de esa cara (el vecino está dibujado) aunque los dos ya no
coinciden en sus celdas: se vería el cielo por la costura. Por eso un resultado cuyo vecino de
cara del mismo nivel espera rearmarse queda guardado, y el grupo sale en el mismo frame cuando
llega el último. Un vecino que sigue cambiando no retiene a los otros más de 2 s, y un nodo que
cierra un hueco no espera.

Los chunks editados por encima de la última capa del generador suben el techo de sus columnas
(sección 6): una plataforma en el cielo tiene nodos en los niveles finos, como una torre.

Sin persistencia, un chunk editado que se descarga vuelve generado, así que sale del conjunto y
sus nodos vuelven a las celdas del generador. Con persistencia queda (está guardado) y su
pirámide se lee del disco cuando hace falta. El nivel 6 (`extreme`) no muestra ediciones: ahí una
celda abarca 8 chunks.

**Respaldo.** Un generador sin `lod()` tiene solo el nivel 1 (`fallback_max_level`, 0 lo apaga),
armado desde sus chunks reales con la regla exacta (`boxel_world::lod::fallback`): el chunk
cargado, si no el guardado, si no el del generador (los generados se guardan por hilo, 64). Un
trabajo arma un nodo y hay uno en vuelo: cada nodo lee hasta 64 chunks. La banda sale de
`vertical_chunks`, que debe estar acotado (64 capas como mucho); si no, el LOD queda apagado
(`NoSource`). El horizonte es `T_1`: 1.536 bloques con `far`. Entre frames sus pasos `Idle` solo
juntan resultados, y no piden otro paso si no hay nada que juntar.

## Números medidos

La campaña completa (RTX y 780M, r16 y r32, con y sin tope, boost, cuevas, spans, cada ronda)
está en [rendimiento/lod-2026-10.md](../rendimiento/lod-2026-10.md).

### Finales

Medidos el 2026-10-11 sobre 3506990 (la rama terminada), con el mismo binario sin y con `--lod`,
la máquina tranquila y nada compilando. La cadena de `perf-smoke` se corrió dos veces y repitió
los pares dudosos: de 2 a 6 pares por caso en la RTX, 4 en la 780M con `far`.

`just perf-smoke --base HEAD "--new-extra='--lod far'" rtx/surface/r16/uncapped
rtx/forest/r16/uncapped` y el resto de los casos (costo = media del frame con LOD menos sin LOD):

| caso, sin tope | fps | p99 ms | costo | frames > 6,94 ms | llenado |
|---|---:|---:|---:|---:|---:|
| RTX, superficie r16 | 2172 a 2204 -> 952 a 964 | 0,99 a 1,09 -> 2,50 a 2,52 | 0,58 a 0,59 ms | 0 -> 0 | 1,2 a 1,3 s |
| RTX, bosque r16 | 3905 a 3931 -> 1173 a 1189 | 0,67 a 0,71 -> 2,14 a 2,17 | 0,59 a 0,60 ms | 0 -> 0 | 1,1 a 1,2 s |
| RTX, boost r16 | 3825 a 3863 -> 1176 a 1185 | 0,73 a 0,75 -> 2,15 a 2,23 | 0,58 a 0,59 ms | 0 -> 0 | 1,1 s |
| RTX, cueva con ruta r16 | 2748 a 2765 -> 2408 a 2420 | 0,83 -> 0,86 a 0,87 | 0,05 ms | 0 -> 0 | 1,3 a 1,4 s |
| RTX, bajada r16 | 1351 a 1533 -> 1288 a 1289 | 1,79 a 1,98 -> 2,09 a 2,11 | 0,04 a 0,12 ms | 0 a 1 -> 0 | 1,4 s |
| RTX, superficie r32 | 743 a 746 -> 530 a 532 | 3,16 a 3,22 -> 4,68 a 4,70 | 0,54 ms | 0 -> 0 | 2,2 s |
| RTX, bosque r32 | 1699 a 1710 -> 865 a 868 | 1,59 a 1,62 -> 2,68 a 2,73 | 0,56 a 0,57 ms | 0 -> 0 | 1,6 s |
| RTX, bajada r32 | 369 -> 352 a 354 | 6,23 -> 6,53 a 6,64 | 0,12 a 0,13 ms | 1 a 2 -> 1 a 3 | 3,1 s |
| RTX, `near`, superficie r16 | 2167 a 2181 -> 1365 a 1528 | 1,02 a 1,05 -> 1,80 a 2,04 | 0,19 a 0,27 ms | 0 -> 0 | 0,85 s |
| RTX, `near`, bosque r16 | 3854 -> 2198 | 0,71 -> 0,98 | 0,20 ms | 0 -> 0 | 0,76 s |
| RTX, `extreme`, superficie r16 | 2085 a 2185 -> 734 | 1,04 a 1,09 -> 3,21 a 3,23 | 0,88 a 0,91 ms | 0 -> 0 | 1,6 a 1,7 s |
| RTX, `extreme`, bosque r16 | 3872 a 3903 -> 866 a 867 | 0,69 a 0,70 -> 2,70 a 2,71 | 0,90 ms | 0 -> 0 | 1,5 a 1,6 s |
| 780M, `far`, superficie r16 | 530 a 548 -> 335 a 342 | 4,22 a 4,55 -> 7,14 a 7,26 | 1,04 a 1,14 ms | 0 -> 59 a 85 | 1,5 s |
| 780M, `far`, bosque r16 | 914 a 927 -> 459 a 479 | 2,56 a 2,64 -> 5,17 a 5,42 | 1,00 a 1,10 ms | 0 -> 0 a 1 | 1,1 a 1,2 s |
| 780M, `near`, superficie r16 | 531 -> 444 | 4,40 -> 5,38 | 0,37 ms | 0 -> 0 | 0,8 s |
| 780M, `near`, bosque r16 | 931 -> 655 | 2,60 -> 3,80 | 0,45 ms | 0 -> 0 | 0,6 s |

- **La compuerta del plan en la RTX:** ningún frame de más de 6,94 ms con `far` (se cumple en
  todos los casos) y el p99 dentro de 1,5 veces (no se cumple a r16: 2,3 a 3,2 veces; a r32, de
  1,44 a 1,71). El smoke sale con 1 por la regla del 85 % de fps y por ese p99: es el precio del
  plugin, como la niebla (ADR 0030). Sin tope la GPU limita: el frame sin LOD es de 0,26 a
  0,46 ms y el LOD suma 0,6 ms de triángulos que se ven. `near` en el bosque queda en 1,37 veces.
- **Con tope 144** (solo pares en el mismo modo de salteo de la NVIDIA, issue #5; 3 pares por
  caso): a r16 y en el bosque a r32 el LOD no cambia los frames de más de 7,5 ms (bosque r32:
  5 -> 13, 9 -> 8, 12 -> 9). La superficie r32 suma unos 14 (30 -> 44) y la bajada r32 de 0 a 11
  (138 -> 137, 140 -> 151, 143 -> 149). Con LOD la superficie r32 cae en el modo alto en las 3
  corridas (sin LOD, en 1). Son refrescos salteados en los frames de la puesta al día.
- **Hilo principal** (`stress --spans`, frames normales): `render.lod` 0,007 ms de media sin
  tope y 0,026 con tope 144 a r32 (0,16 en la puesta al día); `world::lod_coverage` 0,009 y
  0,05 ms; `world::lod_collect` 0,003 y 0,018 ms; `world::lod_edits` 0,001 ms. Por borde, unos
  1,6 ms en total repartidos en unos 9 frames. La puesta al día del mundo por borde queda dentro
  de 1 ms de sin LOD sin tope.
- **La 780M** no tiene que pasar (decisión del dueño). `near` no deja frames de más de 6,94 ms;
  `far` cuesta 1,0 a 1,14 ms y su p99 queda justo en 7,1 a 7,3 ms.

`headless_world --world forest --lod PRESET` (sin GPU, tres corridas; entre paréntesis sin LOD):

| corrida | nodos | quads | dibujados | llenado | trabajo p50 / p99 |
|---|---:|---:|---:|---:|---:|
| `far`, r16 | 8.841 | 8,33 M | 7.095 | 0,91 a 0,94 s | 260 a 270 / 630 a 690 us |
| `far`, r32 | 8.841 | 8,33 M | 6.139 | 1,31 a 1,34 s | 270 a 280 / 630 a 690 us |
| `far`, r16 `--fixed-height` | 8.841 | 8,33 M | 7.095 | 0,91 s | 260 / 590 us |
| `far`, r16 `--fixed-height`, ojo a y 3.000 | 7.894 | 7,15 M | 6.392 | 0,83 s | 270 / 600 us |
| `near`, r16 | 3.191 | 2,91 M | 2.449 | 0,43 a 0,45 s | 240 a 250 / 600 a 820 us |
| `extreme`, r16 | 12.713 | 11,89 M | 9.999 | 1,25 a 1,28 s | 280 a 290 / 620 a 680 us |

- Update más lento al arrancar: r16 1,88 a 2,45 ms (1,87 a 2,17); r32 3,55 a 4,00 ms (2,56 a
  3,33). En un salto de 1.000 chunks: r16 3,36 a 4,06 ms (2,36 a 2,99); r32 4,28 a 4,43 ms
  (3,76 a 4,03). Volando a 100 bloques/s a r16, p99 del update 1,95 a 2,06 ms (1,42 a 1,59).
- Los nodos del piso son 0 en los mundos del ejemplo. En el terreno sin árboles son 8.551 nodos y
  3,39 M quads: los árboles son más de la mitad.
- Criterion `lod/column` (bosque, sin cambios desde la ronda 3): 256 us en el nivel 1 (la fuente
  sola, 65 us) y 227 us en el nivel 4.

### Antes de optimizar (M1)

Las tablas de esta parte son del M1, antes de las rondas de optimización: sirven para ver qué
cambió (`render.lod` de 0,29 a 0,007 ms, el costo de GPU de 1,1 a 0,6 ms, el llenado de 2,4 a
1,1 s).

`headless_world --world forest --lod PRESET` del 2026-10-11 sobre la rama (sin GPU: solo el lado
del mundo; la máquina tranquila). Los nodos del piso son 0 en todas las corridas.

| corrida | columnas | nodos | quads | dibujados | llenado | trabajo p50 / p99 | update p99 / máx |
|---|---:|---:|---:|---:|---:|---:|---:|
| `far`, r16 | 7.692 | 8.841 | 8,33 M | 7.095 | 2,36 s | 260 / 780 us | 0,78 / 0,84 ms |
| `far`, r32 | 7.692 | 8.841 | 8,33 M | 6.139 | 2,75 s | 260 / 720 us | 0,58 / 2,86 ms |
| `far`, r16 `--fixed-height` | 7.692 | 8.841 | 8,33 M | 7.095 | 2,35 s | 260 / 680 us | 0,70 / 0,87 ms |
| `near`, r16 | 2.648 | 3.191 | 2,91 M | 2.449 | 0,94 s | 230 / 680 us | 0,41 / 0,63 ms |
| `extreme`, r16 | 11.564 | 12.713 | 11,89 M | 9.999 | 3,32 s | 280 / 720 us | 0,70 / 0,81 ms |

- La compuerta del plan (antes de escribir código de GPU) pedía menos de 14.000 nodos y 10 M de
  quads en `far` a r16: se cumple, así que `R` sigue en 24.
- Por nivel en `far` a r16 (el bosque de la tabla): el nivel 1 tiene 3.085 nodos (543 quads por
  nodo, 357 vacíos), el 2 tiene 1.936 (1.033), el 3 tiene 1.936 (1.418) y el 4 tiene 1.884
  (1.009). En el terreno sin árboles (`--world terrain`) son 8.551 nodos y 3,39 M quads: los
  árboles son más de la mitad.
- El llenado tarda unos 2,4 s y no 1 s como estimaba el plan. El límite es la cadencia del paso
  `Idle` entre frames (cada paso dura unos 0,9 ms en este portátil: el `sleep` de 250 us del
  runner se pasa), no los trabajos. El nivel más grueso, el horizonte, llega primero.
- Bench de criterion `lod/column` en el bosque (una corrida, release): en el nivel 1, la fuente
  sola tarda unos 65 us y el mallado con las pasadas unos 190 us por nodo.
- Costo en el hilo principal, `headless_world` volando a 100 bloques/s: p99 de 1,97 a 2,09 ms con
  el LOD contra 1,63 a 1,68 ms sin él. Lo más caro son los frames donde carga una losa de chunks
  (la cobertura: de 0,15 a 0,6 ms) y los replaneos completos cada 32 bloques.
- El primer plan cuesta unos 3 a 4 ms en `far` y 6 ms en `extreme`, dentro de los frames de
  carga. Un salto (teletransporte) cuesta lo mismo una vez.

En la RTX 5050, `voxel_world --lod far` a r12 se mantiene en 144 fps con tope (6,9 ms de media),
con unos 2.100 a 2.200 draws del LOD por frame.

`just perf-smoke --base HEAD "--new-extra='--lod far'" rtx/surface/r16/uncapped
rtx/forest/r16/uncapped` del 2026-10-11 sobre 7e2557c (y lo mismo con `780m/surface/r16/uncapped`):
el mismo binario sin y con el LOD, 10 s por lado, radio 16, sin tope. El harness repitió cada
par y dio lo mismo. El llenado terminó a los 2,6 a 2,7 s (`lod_fill_s`), antes de empezar a medir
(el mundo se asentó a los 0,7 s y hay 2 s de calentamiento).

| caso | fps | p50 ms | p99 ms | frames > 6,94 ms |
|---|---:|---:|---:|---:|
| RTX, superficie, sin tope | 2024 -> 694 | 0,44 -> 1,12 | 1,43 -> 3,92 | 1 -> 0 |
| RTX, bosque, sin tope | 3655 -> 808 | 0,24 -> 1,03 | 0,78 -> 3,22 | 0 -> 0 |
| 780M, superficie, sin tope | 538 -> 286 | 1,76 -> 2,72 | 4,52 -> 8,25 | 0 -> 286 |

La 780M no tiene que pasar (decisión del dueño): con `far` cuesta 1 a 1,2 ms en la mediana (el
plan estimaba de 2 a 3,5) y unos 290 a 400 frames de cada 10 s pasan de 6,94 ms. Para mostrarlo
en ella va el preset `near`.

El smoke sale con 1 por la regla del 85 % de fps, que es el precio del plugin (como con la niebla,
ADR 0030), y también por el p99: el plan pedía en la RTX ningún frame de más de 6,94 ms (se cumple)
y el p99 dentro de 1,5 veces (no se cumple: unas 3 a 4 veces). El LOD cuesta unos 0,7 ms por frame
en la mediana y lleva el p99 a 3 a 4 ms, siempre debajo de un frame de 144 Hz.

Dónde va ese tiempo: la misma superficie con `--features trace --spans` (frames normales, ms):

| span | media | p99 | máx |
|---|---:|---:|---:|
| frame | 1,41 | 3,74 | 5,73 |
| `render.lod` (lista de draws y su encoding, CPU) | 0,29 | 0,61 | 1,34 |
| `render.offscreen_wait` (esperar a la GPU) | 0,47 | 2,57 | 3,73 |
| `world::lod_coverage` | 0,01 | 0,04 | 0,47 |
| `world::lod_collect` | 0,002 | 0,009 | 0,03 |

- El p99 es casi todo espera de la GPU: con el LOD la RTX dibuja unos 2 M de quads más por
  frame. El M3 (el camino de GPU de High) baja el costo de CPU, no ese.
- `render.lod` queda en 0,29 ms de media, debajo de los 0,5 ms con los que el plan adelantaba el
  M3 antes del M2. Su p99 (0,61 ms) sí pasa.
- En el lado del mundo, al cruzar un borde de chunk se suman por cruce unos 0,7 ms de
  `lod_plan` y 0,6 ms de `lod_coverage`, repartidos en unos 8 frames.

### Ediciones (M2)

Las ediciones no cambian el costo de un mundo sin ediciones. `just perf-smoke --base
stress-7efef4b rtx/surface/r16/uncapped rtx/forest/r16/uncapped -- --lod far` del 2026-10-11 (el
binario de antes del M2 contra a5ac6b6, los dos con `--lod far`, la máquina tranquila):

| caso | fps | p99 ms | frames > 6,94 ms |
|---|---:|---:|---:|
| RTX, superficie, sin tope | 959 -> 958 | 2,53 -> 2,58 | 0 -> 0 |
| RTX, bosque, sin tope | 1185 -> 1172 | 2,16 -> 2,17 | 0 -> 0 |

## Desvíos del plan

El plan se siguió en lo esencial. Donde el código pidió otra cosa, se siguió el código y la
intención del plan:

**Regla de la celda y mallado (`boxel_voxel`, `boxel_mesh`)**

- Las paredes de agua del nivel 1 cubren más que "el agua hasta el primer sólido": toda celda no
  vacía de uno de los 9 planos, con la celda de enfrente no vacía y agua a 2 celdas o menos, si no
  es ya un faldón. La prueba de rayos contra chunks reales encontró rayos bajo el agua cercana que
  se colaban por el costado del fondo lejano (166 en un terreno): la regla redondea la celda de
  enfrente a suelo y no había cara. Con la regla amplia no se cuela ninguno; cuesta unos quads más
  en el nivel 1 cerca del mar.
- Las cajas y losas translúcidas cuentan como vacías, como el vidrio. Los estados vacíos usan el
  material 0 (aire).
- Con dos fluidos distintos en una celda, la mezcla conserva el de más bloques (empate: el id
  menor). Esa mezcla es conmutativa pero no asociativa; `of_cell` cuenta cada fluido aparte. El
  proptest de asociatividad usa un solo fluido, como permite el plan.

**Fuente del ejemplo (`boxel_demo`)**

- El suelo de una celda es el máximo de sus 4 esquinas más un margen: 0 en el nivel 1, 1 bloque
  en los niveles 2 a 4 y `s/16` (hasta 8) más arriba. Sin margen, la cima gruesa quedaba bajo la
  exacta en 1 de 4.000 celdas en el nivel 2 y en 5 de 4.000 en el nivel 3, y el test lo prohíbe.
  El estado de la superficie (pasto, arena, tierra, piedra) sigue el máximo sin margen, así una
  playa a 48 sigue siendo arena.
- Desde el nivel 3, si crece un árbol se decide con el máximo de las esquinas sin margen, pero el
  árbol se apoya en el suelo con margen. Con margen en las dos cosas crecían árboles en las
  playas; sin margen en las dos, una copa quedaba bajo la real a 2^36.
- Entre árbol y suelo, en la celda del máximo de las esquinas o más arriba gana el bloque de árbol
  tan alto como ese máximo (hojas sobre tronco). El margen hacía ganar al suelo y se perdían 2 de
  144 árboles en el nivel 2.
- La banda se elige para que toda capa bajo `lo` sea piedra exacta en todo su bloque acolchado.
  Dentro de la banda, un nodo sin aire (borde incluido) y sin fluido se emite `Uniform(piedra)`
  aunque sus celdas tengan tierra o pasto: nunca se dibuja nada de él. Así la capa del piso no se
  malla en los niveles 5 y 6. Nota para el M2: un nodo así, si una edición lo necesita, debe
  llenarse con sus estados reales.
- El test de `REPEAT_OFFSETS`: el terreno no se repite, así que "columnas desplazadas dan celdas
  idénticas" se probó como acuerdo de bordes con los 4 vecinos y las capas de arriba y abajo, a
  256, 2^36 y 2^40, más la cima gruesa contra la exacta ahí.
- `LodRequest.dim` no se usa (una sola dimensión).

**Lado del mundo (`boxel_world::lod`)**

- Un quinto archivo, `tree.rs`, guarda los nodos, la disponibilidad, los bits de hijos cubiertos
  y tocados, las máscaras, las ops y los conteos. `coverage.rs` guarda solo la frontera con los
  chunks reales.
- La cadena de sistemas corre después de `WorldSystems::Visibility` y no solo después de
  `Collect`, para no ser ambigua con la visibilidad (que escribe el `ChunkMap`).
- **La regla de la cadena reemplaza "conservar hasta reemplazar".** Con las definiciones del plan,
  un nodo de nivel 1 podía dibujar dentro del octante de un abuelo de nivel 3 mientras el nodo de
  nivel 2 entre ellos no estaba listo: dos niveles sobre el mismo chunk. El proptest de cobertura
  lo encuentra apenas se quita la regla. Ahora un nodo se envía solo con toda su cadena de
  ancestros disponible, y un nodo rechazado retira a sus descendientes; un nodo que sale del árbol
  se quita enseguida (su padre siempre está dibujado).
- `ChunkMeshResync` reconstruye lo publicado con `ready_meshes()` más los chunks que el LOD tenía
  publicados, que se están volviendo a mallar y siguen con `ChunkSlot::shown`. Con
  `ready_meshes()` solo, un chunk cuyo remallado empezó entre la subida y la lectura del LOD
  quedaría tapado por el LOD hasta su malla nueva. Error que queda: si en ese mismo frame su
  `shown` pasó a falso por una edición o por un vecino descargado, hay una superposición corta.
- La lista de vigilancia de chunks vacíos mira solo los chunks cuyo octante de nivel 1 tiene
  quads, un cuarto de la lista por frame (entre 64 y 1024). Los chunks sin interés de malla quedan
  en ella: de 172 a 378 entradas en `far` a r16 y r32.
- Generaciones: cada trabajo tiene un id único y una columna acepta solo el resultado del id que
  tiene en vuelo. Funciona igual que un contador por columna y sobrevive a una columna que se
  borra y se vuelve a crear.
- El tope de trabajos en vuelo cuenta al mundo como asentado solo con `WorldStats.chunks > 0` y
  `is_settled()`: antes de sus primeras estadísticas todo vale cero y parecería asentado.
- `LodSettings` ganó `frame_budget` (0,3 ms). `horizon()` recibe la región de conservación;
  `preset_horizon()` y `thresholds()` son nuevos.
- **Replaneo por nivel.** El plan estimaba menos de 0,1 ms por replaneo; con mapas hash costaba
  de 1,5 a 3 ms (p99 de un vuelo a 100 bloques/s: 3,96 ms). Ahora cada nivel es una ventana densa
  y el nivel `k` se vuelve a planear cuando el ojo se movió `max(8, e_(k-1)/8)` bloques (8 para el
  nivel 2, 16 para el 3, 32 para el 4). El nivel 1 sigue al 2 y todo se rehace si cambia un
  umbral. Los niveles gruesos se atrasan a lo sumo un octavo de su banda de histéresis.
- **Estadísticas.** Los conteos por nivel se llevan al cambiar los nodos, nunca recorriendo:
  recorrer todo costaba hasta 2,4 ms por frame en `far`. `LodStats.fill` es la parte de las
  columnas cuya banda se conoce, tope 0,99 hasta que todo está armado y enviado.
- Ops que nadie toma en el `Update` siguiente se descartan (herramientas sin renderer).
- Clases de prioridad: la clase 0 es el nivel más grueso y toda columna cuyo trabajo arma un nodo
  que cierra un hueco, de cualquier nivel (los ancestros vienen con él, como pide la regla de la
  cadena). Un nodo de nivel 1 solo tocado, con todos sus chunks cubiertos, es clase 3. La
  prioridad es por columna.
- Apagar el LOD (desactivado, la guarda, sin fuente, ajustes inválidos, sin ojo) quita todos los
  nodos y cancela los trabajos; prenderlo otra vez llena desde cero.
- Un árbol nuevo (primer plan, cambio de contenido, LOD prendido otra vez) lee toda la cobertura
  una vez. Si no, un LOD prendido sobre un mundo ya cargado dibujaría encima.
- Orden del resync: primero los nodos de nivel 1 que cierran un hueco con sus ancestros, después
  el resto, cada parte del más grueso al más fino.
- Una capa uniforme cuenta como conocida solo mientras su columna la quiere. Antes, al soltar una
  columna se olvidaba su banda y el estado de cubierto de sus capas cambiaba sin avisar al padre.

**Render (`boxel_render`, `boxel_shaders`)**

- Subidas: un buffer de staging por frame y una copia por nodo, como las subidas principales. El
  plan decía `queue.write_buffer` por nodo, pero cada llamada crea su propio staging.
- La compuerta de GL vive en `VoxelRenderer::set_lod` (con `adapter_info`), así el camino sin ECS
  también queda apagado. El smoke se salta el LOD en GL.
- `LodInput` es `{ settings, horizon: Option<f32>, ops, resync }`. `horizon` es `None` mientras el
  mundo no dibuja nada; `resync` tira todos los nodos antes de las ops.
- Un `Upsert` rechazado para un nodo ya presente también tira su malla vieja (el mundo lo trata
  como fuera del renderer). Claves que no son nodos (lod 0, lod mayor que 15, origen fuera de
  `i64`) también se rechazan. La tabla de nodos tiene a lo sumo 65.536.
- Los draws del LOD no suman en `RenderStats::draws`: van en `lod_draws`. Los nodos a más de 2^30
  bloques del origen de render no se dibujan (la ventana de la GPU, como los chunks).
- El ordenador de translúcidos también usa la vista con el plano lejano en `H`; si no, los chunks
  translúcidos de la banda de conservación se dibujarían sin volver a ordenarse.
- Ganchos de test ocultos: `VoxelRenderer::debug_view_uniform()` y
  `RendererSmokeTest::debug_fail_lod`.

**Conexión y ejemplos (este paso)**

- `extract_lod` recibe del `extract` del render un resumen del renderer (`LodRenderer`: si el LOD
  puede dibujar ahí y con qué arena se armó su pasada), así se prueba sin GPU. Un dispositivo
  perdido se ve como "sin renderer".
- Sin renderer, `extract_lod` descarta las ops pero no escribe `LodResync` (el plan pedía
  escribirlo). Todo renderer, nuevo o rearmado después de perder el dispositivo, empieza con un
  frame de resync, y ese frame lo escribe. Escribirlo en cada frame sin renderer haría que el
  mundo reenvíe todo una y otra vez mientras no hay GPU.
- `GpuState` pasó a `pub(crate)` para ordenar `extract_lod` entre `extract` y
  `extract_visibility`.
- `RenderInfo` ganó `lod_nodes`, `lod_draws` y `lod_quads` (para la línea de F3).
- `stress` mide el llenado desde el arranque de la corrida (`lod_fill_s`), como `settle_s`.
- La pose alta del test de costura usa un radio de 12 chunks: con radio 4 los chunks que la
  búsqueda vieja perdía quedaban debajo de la imagen.

**El M3 y las rondas de optimización**

- El M3 vino antes que el M2. Sus listas se parten por rango de lugares (10.922 por lista), no
  por nivel. La geometría del LOD va en el grupo 3, y los draws translúcidos de High ligan su
  grupo 2 y su buffer de índices (se tocó `path/high.rs`), así la pasada principal se puede
  partir.
- La profundidad jerárquica (el M5 del plan) está hecha, solo en High, y se usa según lo que
  tapa: si tapa a lo sumo 1 de cada 16 nodos, 30 frames sin ella. La pasada principal se corta
  después de lo opaco y cutout, y el depth target se puede muestrear mientras existe el camino
  de GPU.
- `lod.wgsl` se partió en `lod_common`, `lod`, `lod_pull`, `lod_cull` y `lod_hiz`. El vertex del
  LOD tiene su propia salida (`LodOut`, la misma cuenta) en vez del `VsOut` y `shade_fragment` de
  `common`.
- High usa el camino instanciado en las GPU integradas, y el instanciado dibuja una llamada por
  tramo de baldes visibles, no una por nodo.
- Los faldones y las paredes de agua se dibujan solo donde la regla de costura los muestra. Cada
  quad lleva su costura en el índice de la paleta de luz (3 entradas de cielo abierto); los de
  costura van al final de cada balde, primero los de las caras del nodo y después los de los
  planos internos. `LodNodeGpu` mide 64 B, con los conteos de costura por balde, y los bits de
  vecino van en `node.w`, bits 15 a 20.
- En High, `RenderStats.lod_draws` cuenta listas indirectas y `lod_quads` los quads de los nodos
  dibujados antes del recorte en la GPU.
- Mientras el mundo carga solo corren columnas de clase 0 (el plan decía 2 trabajos de cualquier
  clase). Los hilos encadenan trabajos armados de antemano. `publish_per_frame` pasó de 64 a 192.
- Un solo presupuesto por frame para el replaneo, los trabajos y los eventos de datos, dividido
  por 3 mientras el mundo carga; con el mundo asentado los trabajos tienen el suyo. Los eventos de
  datos se leen dentro del presupuesto, no completos (los de mallas sí, completos), y el llenado
  también espera esa cola.
- Un nivel se planea de a filas: el plan estimaba menos de 0,1 ms por nivel y se midieron hasta
  0,69 ms.
- Los viewers más allá del horizonte del preset quedan fuera de la región de conservación. Antes,
  un salto encendía la guarda y tiraba todos los nodos.
- La lista de vigilancia tiene una parte dormida para los chunks sin malla pedida.
- Un nodo que sale del árbol saca del renderer a sus descendientes enviados (un replaneo en varios
  frames podía dejar nodos de nivel 1 bajo un padre quitado después de un salto).
- `snapshot` sube todos los nodos en su único frame. Antes sus imágenes tenían cuadrados de cielo
  del tamaño de un nodo, distintos en cada corrida.

**Ediciones y respaldo (M2)**

- Las pirámides arman el nivel 1 desde los bloques y cada nivel de arriba mezclando los 8 de
  abajo (`CellPartial::merge`), no cada nivel desde los bloques. Es exacto con un solo fluido por
  celda; con dos, la mezcla conserva el de más bloques, como en el M1.
- Una columna cuyos chunks editados no tienen la pirámide en caché espera fuera de la cola a un
  trabajo de pirámide, en vez de dibujar primero lo del generador y corregirlo después. El "8
  chunks editados por trabajo" del plan quedó en los trabajos de pirámide (leen 8 chunks como
  mucho, 2 en vuelo, fuera del tope de las columnas); una columna pone todas las pirámides que
  toca, que ya están calculadas.
- Las bandas cambian con las ediciones: una columna crece hasta las capas que alcanzan sus chunks
  editados (una torre sobre la banda de la fuente) y vuelve a achicarse cuando la edición se va.
  Las capas que entran tenían nada dibujado: entran como nodos vacíos y se rearman. El plan
  suponía celdas dentro de la banda.
- Un nodo viejo que el renderer tiene se reemplaza en las ops del frame en que llega su malla
  nueva, fuera del tope de envíos por frame (son pocos y chicos). Un nodo vacío que gana malla
  sale enseguida si su cadena de padres está disponible.
- Un trabajo en vuelo de una columna cuyos bloques cambiaron se cancela: leyó bloques viejos.
- `BlockChanged` no trae dimensión: las ediciones son de la dimensión actual del mundo.
- El respaldo lee 4 chunks por eje (64 por nodo), no 3: el borde de una celda de 2 bloques cae en
  el chunk vecino. Un nodo por trabajo, uno en vuelo y sin pasarlos a los hilos de antemano: el
  trabajo copia los chunks cargados al crearse, solo en `Update` (no en los pasos `Idle`). Los
  chunks fuera de `vertical_chunks` son aire, como los hace el mundo, y el piso real queda a la
  vista (la fuente del ejemplo lo cuenta como piedra).
- El respaldo es el preset con `levels` recortado a `fallback_max_level`: el horizonte es `T_1`.
- `FlatGenerator::lod()` devuelve su fuente exacta, así que un mundo plano también tiene LOD.
- `LodStats` ganó `edited_chunks`, `pyramids` y `fallback`.
- Un error que encontró el test del respaldo: el LOD se daba por lleno con columnas marcadas para
  rearmar que nadie había puesto en la cola. Ahora esas columnas cuentan como trabajo pendiente.
- El plan no quería capas por encima de la última capa del generador ("ahí todo es aire"). Con
  ediciones eso deja de ser cierto: un chunk editado más arriba sube el techo de su columna en
  cada nivel. Sin esto, una plataforma sobre y 128 no aparecía en los niveles 1 y 2, y con el
  respaldo no aparecía nunca.
- Los vecinos del mismo nivel que se rearman por una edición cambian juntos en el renderer
  (sección 11). El plan no lo pedía: el corte de faldones de la ronda 3 supone que dos vecinos
  dibujados salen de las mismas celdas.

## Límites conocidos

- **Ediciones.** Se ven hasta el nivel 5; en el 6 (`extreme`) no. En la costura entre una celda
  editada y una generada puede haber una celda de diferencia. La fuente del ejemplo emite como
  piedra uniforme un nodo sin aire ni fluido (desvío del M1): si una edición cava en él, las
  paredes del pozo que caen fuera del chunk editado se ven de piedra, no de tierra.
- **Muchas ediciones juntas.** Una columna que toca más chunks editados que la mitad de la caché
  (1.024) no espera las pirámides que faltan: se arma con las que hay.
- **Generadores sin fuente.** Solo el nivel 1, y solo con `vertical_chunks` acotado. Muestra el
  piso real del mundo (sus caras de abajo) donde la fuente del ejemplo pone piedra.
- **Agua opaca.** El mar lejano es un color opaco: en un mar calmo se ve un escalón de color en el
  borde del detalle completo (decisión 5; las caras translúcidas serían M4).
- **Capas de niebla.** Una capa que se desvanece al color del cielo en su propio alcance deja una
  franja pálida sobre el terreno lejano (decisión 6; M4).
- **Bajo tierra.** Dentro de una cueva sellada el LOD no se dibuja. En una cueva abierta lo
  recorta la pirámide de profundidad en High y el early-Z de la roca en Mid y Low.
- **Bajo el piso del mundo.** Debajo de y = -64 el LOD no tiene cara de piso: nadie vuela ahí.
- **Cuevas y plantas.** Lejos, las cuevas se cierran y las plantas no están. Las hojas son opacas,
  con el color medio de sus píxeles opacos: los bosques lejanos se ven algo más densos.
- **Acantilados.** Los faldones cubren pendientes de hasta 3 a 1; un acantilado vertical puede
  dejar una grieta de un píxel.
- **Varios viewers.** El LOD se refina alrededor de la cámara: otro viewer lejos queda como una
  isla con huecos cortos en su borde. `voxel_world` tiene uno solo.
- **Huecos cortos.** Un octante de nivel 2 o más con chunks publicados espera a su hijo de nivel 1:
  unos frames de cielo al arrancar o después de un salto. Después de un salto dentro del
  horizonte, el borde de la zona vieja también muestra cielo hasta que el mundo la descarga
  (menos de un segundo): esos chunks quedan lejos de `T_1` y nadie arma sus nodos de nivel 1.
  Un chunk sin dibujar porque el renderer principal lo rechazó (arena llena, ya un modo de falla
  avisado) deja un hueco de dos frames. Un nodo cuya subida no entra en el frame (ventana recién
  mostrada) o falla (sin memoria) deja su hueco hasta el frame en que sube.
- **Vigilancia de chunks vacíos.** Los chunks vacíos en octantes sin quads del LOD no se buscan:
  no cambian lo que se dibuja, pero no cuentan como cubiertos.
- **Árboles en los niveles 5 y 6.** Se mira uno de cada 4 y de cada 16 lugares de árbol.
- **p99 sin tope.** En la RTX a r16 queda de 2,3 a 3,2 veces el de sin LOD (a r32, de 1,4 a 1,7):
  son los triángulos que de verdad se ven. Es el precio del plugin, como la niebla.
- **Con tope 144** la superficie a r32, con la NVIDIA en su modo alto, suma unos 14 frames de más
  de 7,5 ms por corrida, y la bajada a r32 hasta 11: refrescos salteados en los frames de la
  puesta al día (issue #5). Con el LOD la NVIDIA cae más seguido en ese modo.
- **Costo en el hilo de render.** En High casi nada (una lista indirecta por segmento). En Mid,
  Low y las GPU integradas, una llamada por tramo de baldes visibles de cada nodo visible.
- **El llenado** tarda de 1,1 a 1,7 s; en la superficie a r32, 2,2 s, y en la bajada a r32,
  3,1 s, porque espera que el mundo se asiente.
- **Rearmados por ediciones.** Un nodo cuyo vecino del mismo nivel espera rearmarse sigue con su
  malla vieja hasta que llegan los dos, a lo sumo 2 s.

## Alternativas descartadas

- **Mips desde los chunks reales en todos los niveles.** Unos 90 ms por columna de nivel 4 y
  esperar a cargar chunks lejos. Quedan solo para ediciones y el respaldo (M2).
- **Regla de mayoría en la celda.** Los bosques se aplanan en una alfombra y las torres
  desaparecen pasados unos 2 km. DH y Voxy usan "gana cualquier sólido", como acá.
- **"Gana cualquier sólido" sin la excepción del agua.** A 3 km los mares poco profundos se
  volverían arena.
- **Columnas 2D (alturas por columna) en vez de nodos 3D.** No ven bien desde arriba ni las
  formas con aleros; los nodos 3D sirven a cualquier altura de cámara.
- **"Un nodo de nivel 2 o más nunca dibuja sobre un chunk dibujado"** en vez de máscaras por
  octante en todos los niveles: deja anillos vacíos al arrancar.
- **Bias de profundidad** para superponer LOD y detalle completo: z-fighting a distancia y
  costuras raras. Con las máscaras nunca dibujan el mismo volumen.
- **Agrandar `Camera::view_distance` o los `ChunkViewer`:** cambiaría el costo del streaming.
  Solo se mueve el plano lejano del recorte y de la búsqueda de cuevas.
- **Guardar el LOD generado.** Unos 60 MB de redb por zona visitada, sin límite; rearmarlo cuesta
  unos 2 s de CPU de fondo (decisión 8, como ADR 0024 con los techos de cielo).

## Cómo se probó

- `boxel_voxel::lod`: clases (cruz vacía, fluido con su fuente, losa sólida, caja de un cuarto
  vacía, el cubo cutout `test:cutout` sólido); la regla de la celda en todos los casos del plan;
  proptest: la regla por mezclas igual a la directa en grillas al azar hasta el nivel 4, con
  orígenes hasta 2^40, y la mezcla asociativa y conmutativa.
- `boxel_mesh::lod`: materiales; los cortes por octante conservan cada cara y dejan cada quad en
  un octante; faldones y paredes contra referencias de fuerza bruta; las caras de las pasadas no
  se ven desde adentro de un nodo dibujado; pruebas de grietas de 10.000 rayos (nivel 1 contra 2,
  y chunks reales contra nivel 1 con mares de más de 8 bloques, rayos desde arriba y desde abajo
  del agua): ningún rayo se cuela, y sin faldones o sin paredes sí se cuelan.
- `boxel_worldgen`: el `lod()` por defecto es `None`; el orden acolchado; orígenes de nodo hasta
  el límite del mundo.
- `boxel_demo`: el llenado por filas igual a la regla por celda; capas fuera de la banda
  uniformes en todo su bloque; una copa sobre una columna de mar es hojas, no agua; cimas gruesas contra la regla exacta en chunks reales (512
  columnas por nivel, niveles 1 a 3, bosque y overworld); acuerdo de bordes lejos del origen;
  mares de 3 y 10 bloques siguen siendo agua en los niveles 2 a 4; playas de arena; un bosque sin
  mar no tiene agua y un mar subido después de `new()` aparece; el piso; determinismo; las pruebas
  de grietas con la fuente del ejemplo (dos niveles en 5 lugares, chunks reales contra nivel 1 en
  3 lugares con mares de 9 a 13 bloques).
- `boxel_world::lod` (ejecutor en línea): umbrales (proptest: `T_1` cubre la región de
  conservación con radios al azar, también con `u32::MAX` y límites verticales; la guarda); el
  árbol querido contra uno de fuerza bruta, la histéresis, nada pasado `H + e_K`, lejos del
  origen; trabajos (un mensaje por trabajo, pánicos, cancelación, topes 2 y 5, orden de la cola);
  **el invariante de cobertura** (proptest con cambios de cobertura, órdenes de trabajos,
  rechazos, resyncs y movimientos del ojo: cada chunk lo dibuja a lo sumo uno, el detalle
  completo o un nivel del LOD, y con todo listo exactamente uno), y quitar la regla de los tocados
  o la de la cadena lo rompe; la alimentación de la cobertura contra `ChunkCoverage::from_map`;
  5.000 eventos en un frame con presupuesto de 1 ns; streaming real del bosque con un viewer que
  vuela, también con `--fixed-height`. Después de la revisión: bajar el radio de 20 a 2 deja
  cada chunk publicado dentro de un nodo de nivel 1 querido en cada frame de la descarga; las
  capas que vuelven a una columna fallada quedan perdidas; la guarda vuelve solo pasado su
  margen; un ojo a y 3.000 sobre un mundo de altura fija no quiere capas de aire; las clases 0 y
  3; un chunk rechazado que se vuelve a enviar cuenta enseguida y un rechazo viejo no; un
  renderer que no puede dibujar apaga el LOD.
- `boxel_shaders/tests/naga.rs`: `lod.wgsl` valida y traduce a GLSL 330 y ES 300, MSL, HLSL y
  SPIR-V; un uniform block y una textura, sin sampler; el layout de `LodImmediate`; las constantes
  iguales a las de Rust y a `octant_of`; las mips de `textureLoad` en `i32` en todos los módulos.
- `boxel_render` sin GPU: las reglas de `extract_lod` (ops en orden; sin renderer se descartan
  sin `LodResync`; `LodResync` en el frame de resync del renderer nuevo y con otra arena;
  `cannot_draw`); solo los rechazos que dejan un chunk sin dibujar llegan al LOD; las subidas
  atrasadas suben por frames y las perdidas esperan al siguiente;
  `lod_far` es `Some` solo cuando
  `Prepare` dibuja; el foco sigue a la cámara activa; un rechazo llega a la búsqueda de cuevas y
  a `LodFeedback`; la búsqueda con el plano lejano en `H` deja visible todo chunk publicado dentro
  de la vista de recorte, mirando en diagonal sobre la banda de conservación y desde 200 bloques
  de altura mirando 20 grados abajo, y con el plano de la cámara esas poses pierden chunks.
- `boxel_render/tests/gpu_lod.rs`, en cada tier de la RTX 5050, la 780M y llvmpipe:
  - costo cero: sin LOD, apagado, sin horizonte y después de quitarlo, la misma imagen y el
    mismo uniform;
  - los tiers dibujan igual (0,0000 % de píxeles distintos) al mediodía, con tinte y de noche;
  - una máscara de octantes dibuja exactamente el nodo sin esos quads;
  - orígenes lejanos (cruzando ±2^31 y a 2^40) dan la misma imagen;
  - en GL el LOD queda apagado; el smoke pasa en cada tier y una falla apaga el LOD sin tocar el
    tier; cambiar de preset rearma la pasada y la rellena sin rechazos;
  - **costura:** un mundo de bosque real y su LOD, corridos por los plugins hasta asentarse,
    cielo magenta, búsqueda de cuevas como el plugin: ningún píxel magenta donde un rayo llega al
    suelo, a la altura del ojo sobre un mar de más de 8 bloques en la costura, en diagonal sobre la
    banda de conservación y a 200 bloques mirando abajo. Con el plano lejano de la cámara en la
    búsqueda aparecen 87 y 17 píxeles de cielo: el test ve el error;
  - plano lejano y niebla: un chunk pasada la distancia de vista se recorta sin LOD y se dibuja
    con él; la niebla del valle sobre el detalle completo da la misma imagen con y sin LOD;
  - subidas perdidas: un frame sin staging no dibuja esos nodos (la imagen sin nodos) y el
    siguiente los sube y dibuja la escena entera;
  - M3 y rondas: el camino de GPU de High dibuja lo mismo que el instanciado, una búsqueda de
    cuevas sellada no dibuja LOD, y en High los nodos detrás del detalle completo no se dibujan.
- Las suites de GPU existentes siguen verdes sin cambios.
- M2, `boxel_worldgen`: la fuente de `FlatGenerator` es la regla exacta sobre sus propios chunks
  en los niveles 1 a 5 (capas de todas las clases, mar sobre suelo, dos fluidos, lejos del
  origen); sin la cláusula del agua el test falla.
- M2, `boxel_world::lod` (ejecutor en línea, `MemoryStore`): las pirámides son la regla aplicada
  a los bloques en cada nivel; la superposición pone las celdas de su chunk, bordes incluidos,
  también lejos del origen; qué nodos tocan un chunk contra una búsqueda de fuerza bruta; la caché
  saca la menos usada; una columna espera las pirámides que faltan y sigue si el trabajo falla.
  En un mundo que corre: **una torre de un bloque de ancho se ve hasta el nivel 5**, con las celdas
  exactas (la regla sobre los bloques editados) y distinta de la del generador, y los nodos que la
  dibujaban siguen en el renderer en cada frame hasta que llegan los nuevos; sin persistencia, al
  descargarse la torre sus nodos vuelven a los del generador y la capa de aire que había crecido
  se va; con `MemoryStore`, después de descargarse y de vaciar la caché las pirámides salen del
  disco, y después de reiniciar la app lejos de la torre (sus chunks nunca cargan) los chunks
  guardados son editados desde el primer frame y la torre está igual; el respaldo arma solo el
  nivel 1, igual a la fuente exacta del terreno, y muestra una edición.
- Después de la revisión del M2: una plataforma sobre la última capa del generador se ve en los
  niveles 1 y 2, con fuente y con el respaldo, y el techo sube solo en sus columnas; con un
  trabajo por frame, ningún nodo cambia en el renderer mientras un vecino del mismo nivel espera
  rearmarse (fallaba en el frame 3); un paso `Idle` del respaldo no pide otro paso enseguida si
  no puede lanzar nada. El proptest de cobertura pasó con 300 casos.


## Para la prueba del dueño

Lo que miré yo en su lugar, con capturas en la RTX (`snapshot --lod far`, `stress --screenshot`
y `voxel_world --frames`): 34 imágenes en `bench-results/lod-look/` (fuera de git), cada una con
su leyenda en `leeme.txt` y su comando en `comandos.sh`, tomadas de nuevo con el binario final.
Tu pose a r12, r16 y r32, la diagonal, 200 bloques arriba, el mar en el borde, overworld, noche,
atardecer, el tinte, `near` (también en la 780M), `extreme`, `--fixed-height` (también a 3000
bloques), la niebla, el frame donde para un vuelo de 10 s a 100 bloques/s mirando adelante y
atrás, y la torre del M2 vista a 2 km desde un mundo guardado. En ninguna hay cielo bajo el
horizonte, grietas entre niveles ni terreno doble.

Tres cosas que se ven y son esperables:

- Desde 200 bloques, el centro de un lago cercano hondo se ve azul opaco con un borde claro: esa
  capa de chunks queda fuera del radio vertical, así que la dibuja el LOD, con su agua opaca.
- Sobre el mar, el agua lejana opaca queda un bloque más alta que la cercana (celdas de 2 bloques
  en el nivel 1); cerca del borde se nota como un escalón con forma de chunk.
- Desde muy alto (1500 bloques), el bosque del nivel 2 se ve más tupido que el del nivel 1: con
  "cualquier sólido gana" cada copa ocupa celdas enteras (decisión 4).

### Qué mirar en el vuelo del dueño

```bash
cargo run --release --example voxel_world -- --adapter nvidia --lod far --timeout 1800
cargo run --release --example voxel_world -- --adapter nvidia --lod far --view-distance 16 --timeout 1800
cargo run --release --example voxel_world -- --adapter nvidia --lod far --world overworld --timeout 1800
cargo run --release --example voxel_world -- --adapter nvidia --lod far --fog valley --timeout 1800
cargo run --release --example voxel_world -- --adapter nvidia --lod far --fixed-height --timeout 1800
cargo run --release --example voxel_world -- --adapter nvidia --lod far --world-dir /tmp/mundo-lod --timeout 1800
cargo run --release --example voxel_world -- --adapter radeon --lod near --timeout 1800
```

1. Al arrancar, el horizonte aparece en 1 a 2 s, sin anillo vacío.
2. Con boost (Ctrl), girando, en diagonal y desde 200 bloques mirando abajo: sin cielo en el
   borde del detalle completo.
3. El mar lejano azul y las playas de arena. ¿Molesta el escalón del agua opaca en el borde
   (decisión 5) o el bosque más tupido desde muy alto (decisión 4)? Eso sería el M4.
4. F8 recorre far, extreme, oculto y near (el cambio de arena rellena en unos segundos); F9 tiñe
   los niveles; F3 muestra la línea `LOD`; `[` y `]` mueven la hora.
5. `--fog valley` se ve como antes cerca de la cámara.
6. Con `--world-dir`: una torre de 30 bloques y una plataforma de 20 x 20, volar 2 km y verlas;
   reiniciar con el mismo directorio y verlas otra vez.
7. En la 780M con `near`: que se vea bien; no tiene que ir a 144 fps.
8. Decidir el precio en la RTX: el p99 sin tope queda de 2,3 a 3,2 veces a r16 (sin frames de
   más de 6,94 ms). Con tope 144 el LOD no se nota a r16.

## Pendiente

- Decidir con el dueño el p99 sin tope de la RTX: a r16 queda de 2,3 a 3,2 veces el de sin LOD
  (sin frames de más de 6,94 ms), casi todo trabajo de GPU. Las palancas del plan (baldes por
  cara y Hi-Z) ya están; lo que queda es el preset `near` (1,37 a 2 veces) o menos triángulos.
- La prueba del dueño volando, también la del M2 ("Qué mirar en el vuelo del dueño").
- M4, según la prueba del dueño: agua translúcida lejana, niebla que se adelgaza sobre el LOD,
  `textureSample` en el nivel 1 para packs de alta resolución, hojas más oscuras.
- Posibles mejoras de costo: agrupar los eventos de la cobertura por posición de nivel 1,
  cuantizar `T_1` cuando sigue a la región de conservación.

## Consecuencias

- Un juego agrega terreno hasta el horizonte con un plugin y un generador que implemente
  `LodSource`. Sin el plugin, el motor es el de antes.
- `WorldGenerator` tiene un método nuevo con valor por defecto: los generadores existentes no
  cambian.
- La issue #30 queda respondida en lo del LOD: el generador muestrea cada nivel. Sigue abierta
  por el render en hilo propio, la inicialización asíncrona de la GPU y las listas segmentadas.
