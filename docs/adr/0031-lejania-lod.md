# ADR 0031: lejanía con LOD, un plugin opcional

- Estado: borrador. El M1 está hecho en la rama `feat/lod` (sobre `feat/fog`); faltan la prueba
  del dueño y el `perf-smoke` (al final, "Pendiente").
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
- `T_k = max(32 R 2^k, T_(k-1) + 2 e_k)`, y el horizonte `H = T_K`.
- Si `T_1` pasa el horizonte del preset (un radio vertical enorme sin límites), el LOD avisa una
  vez y no dibuja nada (`LodStats.off`).

Un nodo de nivel `k` se refina mientras su caja está a menos de `T_(k-1)` del ojo, con una
histéresis de un nodo hijo. Cada nivel es una ventana densa de columnas, y un nivel se vuelve a
planear cuando el ojo se movió un octavo de su banda (sección "Desvíos").

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
  perdido (falló, o fue rechazado y espera) no oculta: su padre dibuja.
- **La regla de la cadena:** un nodo se envía solo si todos sus ancestros están en el renderer, y
  un nodo rechazado retira a sus descendientes.
- Los nodos de nivel 1 bajo el detalle completo siguen armados: tapan el borde mientras cargan
  los chunks al volar.
- Sin renderer (todavía no hay, o se perdió el dispositivo) `extract_lod` descarta las ops. Todo
  renderer nuevo empieza con un frame de resync; en ese frame, y cuando cambia la arena, el
  renderer tira sus nodos y `extract_lod` escribe `LodResync`: todo nodo deja de estar
  disponible y se vuelve a enviar, del nivel más grueso al más fino.
- Los rechazos del renderer principal llegan por `LodFeedback.chunks`: `extract_visibility` ya
  vacía `take_refused` una vez por frame y ahora también los reenvía.

### 8. Trabajos y presupuesto

- Un trabajo es una columna de un nivel: la fuente, y para cada nodo con celdas el llenado, el
  mallado y las pasadas (a lo sumo 3 nodos). Meta: 0,3 ms.
- Corren en el pool de cómputo asíncrono con un ejecutor propio (el del mundo es privado).
- A lo sumo 2 en vuelo mientras el mundo carga y 5 cuando se asentó: un chunk cercano espera a
  lo sumo un trabajo del LOD por hilo.
- Prioridades: clase 0 el nivel más grueso (el horizonte aparece primero) y los nodos de nivel 1
  con chunks publicados; clase 1 lo que se ve; clase 2 el resto; clase 3 los nodos de nivel 1
  tapados por el detalle completo.
- El hilo principal gasta a lo sumo 0,3 ms por frame en juntar y lanzar trabajos, y envía a lo
  sumo 64 nodos y 2 MiB por frame. Entre frames (ADR 0023) un paso `Idle` sigue juntando y
  lanzando, sin escribir ops.

### 9. Cómo se dibuja

- `LodPass` dentro de `VoxelRenderer`: su propia arena (`VERTEX | COPY_DST`, sub-asignada como la
  principal), así el LOD nunca le quita lugar al detalle completo. Las subidas van en un buffer
  de staging por frame.
- Un pipeline igual en los tres tiers (`lod.wgsl`, módulo nuevo): el quad instanciado del Low, un
  uniform block (`View`), una textura (la mip de 1x1 de cada capa, el color medio), 16 bytes de
  immediates (origen, nivel, máscara, mip, tinte). Una llamada de dibujo por nodo, después del
  recorte en CPU (frustum y la prueba de caras por caja). La identidad entre tiers sale por
  construcción.
- Se dibuja en el pase principal justo después de lo opaco y cutout, antes del agua translúcida
  y de la niebla.
- Luz de cielo abierto (`FULL_SKY_LIGHT`): el ciclo de día y noche funciona; no hay luz de
  bloque lejos.
- Con el LOD dibujando, el renderer mueve el plano lejano del recorte de chunks al horizonte (las
  vistas de recorte de Mid y Low, el frustum del cull de High y el orden de los translúcidos), y
  la búsqueda de cuevas también (`frame_visibility_with`, con el mismo `lod_far` que usa
  `Prepare`). Los chunks publicados llegan más allá de la distancia de vista (la banda de
  conservación y las esquinas de la caja). Si no se dibujaran, quedaría un hueco donde el LOD se
  hizo a un lado.
- La niebla del terreno va de `fade_start * H` a `H` (0,5 por defecto): el detalle completo y el
  LOD se funden juntos con el cielo. `ViewParams` no cambia, así que las capas de `FogPlugin`
  conservan su alcance.
- En GL el LOD queda apagado (sin clip control la profundidad lejana pelea). `voxel_world` lo
  apaga en adaptadores de software, sea cual sea el radio.
- F9 tiñe cada nivel con un color fijo (rojo, verde, azul, amarillo, magenta, cian).

### 10. Ejemplos y herramientas

- `voxel_world --lod near|far|extreme`: F8 recorre far, extreme, oculto y near; F9 tiñe los
  niveles; F3 muestra una línea `lod` (horizonte, nodos, dibujados, draws, quads, trabajos,
  llenado y rechazos).
- `stress --lod PRESET`: el JSON trae `run.lod` y `run.lod_fill_s` (cuándo terminó el primer
  llenado), para separar los frames del llenado.
- `snapshot --lod PRESET`: espera el llenado y lo dibuja; imprime una línea `lod:`.
- `headless_world --lod PRESET`: nodos y quads por nivel, nodos del piso aparte, tiempos de los
  trabajos y del llenado, sin GPU.
- `tools/bench_report.py` cuenta `lod` entre los ajustes; las corridas viejas valen `off`.

## Números medidos

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
- Por nivel en `far` a r16: el nivel 1 tiene 2.795 nodos (306 quads por nodo, 374 vacíos), el 2
  tiene 1.936 (513), el 3 tiene 1.936 (421) y el 4 tiene 1.884 (383). En el terreno sin árboles
  son 3,39 M quads: los árboles son más de la mitad.
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
con unos 2.100 a 2.200 draws del LOD por frame. Falta el `perf-smoke` del plan ("Pendiente").

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
  tocado, de cualquier nivel (los ancestros de los nodos tocados de nivel 1 vienen con ellos,
  como pide la regla de la cadena). La prioridad es por columna.
- Apagar el LOD (desactivado, la guarda, sin fuente, ajustes inválidos, sin ojo) quita todos los
  nodos y cancela los trabajos; prenderlo otra vez llena desde cero.
- Un árbol nuevo (primer plan, cambio de contenido, LOD prendido otra vez) lee toda la cobertura
  una vez. Si no, un LOD prendido sobre un mundo ya cargado dibujaría encima.
- Orden del resync: primero los nodos tocados de nivel 1 con sus ancestros, después el resto,
  cada parte del más grueso al más fino.

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
- `RenderInfo` ganó `lod_nodes`, `lod_draws` y `lod_quads` (para F3 y `stress`).
- `stress` mide el llenado desde el arranque de la corrida (`lod_fill_s`), como `settle_s`.
- La pose alta del test de costura usa un radio de 12 chunks: con radio 4 los chunks que la
  búsqueda vieja perdía quedaban debajo de la imagen.

## Límites conocidos

- **Ediciones.** En el M1 las construcciones del jugador solo se ven en detalle completo; el LOD
  muestra lo que generó el generador (M2).
- **Generadores sin fuente.** Un generador sin `lod()` no tiene LOD (`LodStats.off = NoSource`).
  El respaldo desde chunks reales, solo en el nivel 1, es del M2.
- **Agua opaca.** El mar lejano es un color opaco: en un mar calmo se ve un escalón de color en el
  borde del detalle completo (decisión 5; las caras translúcidas serían M4).
- **Capas de niebla.** Una capa que se desvanece al color del cielo en su propio alcance deja una
  franja pálida sobre el terreno lejano (decisión 6; M4).
- **Bajo tierra.** Hasta el M3 no se salta el dibujo del LOD dentro de la roca: lo tapa el
  early-Z de la roca cercana.
- **Bajo el piso del mundo.** Debajo de y = -64 el LOD no tiene cara de piso: nadie vuela ahí.
- **Cuevas y plantas.** Lejos, las cuevas se cierran y las plantas no están. Las hojas son opacas,
  con el color medio de sus píxeles opacos: los bosques lejanos se ven algo más densos.
- **Acantilados.** Los faldones cubren pendientes de hasta 3 a 1; un acantilado vertical puede
  dejar una grieta de un píxel.
- **Varios viewers.** El LOD se refina alrededor de la cámara: otro viewer lejos queda como una
  isla con huecos cortos en su borde. `voxel_world` tiene uno solo.
- **Huecos cortos.** Un octante de nivel 2 o más con chunks publicados espera a su hijo de nivel 1:
  unos frames de cielo al arrancar o después de un salto. Un chunk que el renderer principal
  rechaza (arena llena, ya un modo de falla avisado) deja un hueco de dos frames; un chunk viejo
  que sigue dibujado con la arena llena se superpone con el LOD.
- **Vigilancia de chunks vacíos.** Los chunks vacíos en octantes sin quads del LOD no se buscan:
  no cambian lo que se dibuja, pero no cuentan como cubiertos.
- **Árboles en los niveles 5 y 6.** Se mira uno de cada 4 y de cada 16 lugares de árbol.
- **Costo en el hilo de render.** Unos 2.000 draws por frame en `far`, uno por nodo, en los tres
  tiers. El camino de GPU para High es del M3 (antes si el `perf-smoke` lo pide).
- **El llenado** está atado a la cadencia de los pasos `Idle`. Si en `voxel_world` se siente lento,
  la palanca es juntar 2 o 3 columnas por trabajo cuando el mundo está asentado (el plan pide
  0,3 ms por trabajo; necesita el visto bueno del dueño).

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
  uniformes en todo su bloque; cimas gruesas contra la regla exacta en chunks reales (512
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
  vuela, también con `--fixed-height`.
- `boxel_shaders/tests/naga.rs`: `lod.wgsl` valida y traduce a GLSL 330 y ES 300, MSL, HLSL y
  SPIR-V; un uniform block y una textura, sin sampler; el layout de `LodImmediate`; las constantes
  iguales a las de Rust y a `octant_of`; las mips de `textureLoad` en `i32` en todos los módulos.
- `boxel_render` sin GPU: las reglas de `extract_lod` (ops en orden; sin renderer se descartan
  sin `LodResync`; `LodResync` en el frame de resync del renderer nuevo y con otra arena);
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
    con él; la niebla del valle sobre el detalle completo da la misma imagen con y sin LOD.
- Las suites de GPU existentes siguen verdes sin cambios.

## Para la prueba del dueño

Lo que yo pude mirar (capturas con `snapshot --lod far` y `voxel_world --lod far --frames`, en la
RTX): el bosque con relieve hasta el horizonte, lagos azules, playas de arena, de día y de noche,
sin anillo ni costura visibles desde 250 bloques de altura; sobre el mar a la altura del ojo, el
agua cercana translúcida y la lejana opaca (el escalón de la decisión 5). Lo que falta mirar
volando:

```bash
cargo run --release --example voxel_world -- --lod far --timeout 900
cargo run --release --example voxel_world -- --lod far --view-distance 16 --timeout 900
cargo run --release --example voxel_world -- --lod far --world overworld --timeout 900
cargo run --release --example voxel_world -- --lod far --fog valley --timeout 900
cargo run --release --example voxel_world -- --lod far --fixed-height --timeout 900
cargo run --release --example voxel_world -- --lod near --adapter radeon --timeout 900
```

- El horizonte aparece en unos 2 s, sin anillo vacío al arrancar.
- Volando con boost (Ctrl), girando, mirando en diagonal y desde 200 bloques mirando abajo: sin
  cielo en el borde del detalle completo.
- Mares poco profundos azules y playas de arena lejos; sin franja pálida donde el mar cruza el
  borde; sin terreno doble ni z-fighting en la costura.
- De día y de noche (`[` y `]` mueven la hora).
- F8 (también de far a extreme, que rearma la arena y rellena), F9 (tinte por nivel) y la línea
  `LOD` de F3.
- `--fog valley` se ve como antes cerca de la cámara.

## Pendiente

- El `perf-smoke` del plan en la RTX y la 780M (`just perf-smoke --base HEAD
  "--new-extra='--lod far'" rtx/surface/r16/uncapped rtx/forest/r16/uncapped`), con sus tablas
  acá; en la RTX, 0 frames de más de 6,94 ms a r16 con `--lod far`. El JSON de `stress` trae
  `lod_fill_s` para separar el llenado.
- M2: ediciones (pirámides por chunk editado, superposición, guardado), el respaldo de nivel 1
  para generadores sin fuente y la fuente exacta de `FlatGenerator`.
- M3: el camino de GPU de High (un multi-draw indirecto por segmento) y saltar el LOD bajo tierra.
- M4, según la prueba del dueño: agua translúcida lejana, niebla que se adelgaza sobre el LOD,
  `textureSample` en el nivel 1 para packs de alta resolución, hojas más oscuras.
- Posibles mejoras de costo: repartir el primer plan en varios frames, agrupar los eventos de la
  cobertura por posición de nivel 1, cuantizar `T_1` cuando sigue a la región de conservación.

## Consecuencias

- Un juego agrega terreno hasta el horizonte con un plugin y un generador que implemente
  `LodSource`. Sin el plugin, el motor es el de antes.
- `WorldGenerator` tiene un método nuevo con valor por defecto: los generadores existentes no
  cambian.
- La issue #30 queda respondida en lo del LOD: el generador muestrea cada nivel. Sigue abierta
  por el render en hilo propio, la inicialización asíncrona de la GPU y las listas segmentadas.
