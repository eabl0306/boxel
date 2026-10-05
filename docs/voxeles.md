# Cómo maneja boxel los vóxeles

Fecha: 2026-10-05. Máquina: Ryzen 7 250 (16 hilos) y RTX 5050 Laptop, Vulkan, tier High.

Sale de dos análisis: uno siguió un chunk por todo el código de boxel y lo midió; el otro leyó
Bevy 0.19.1 y midió en esta misma máquina cómo sube las mallas.

## En resumen

1. Cada chunk de 32³ se guarda comprimido con una paleta. Si es de puro aire, sus vóxeles no
   ocupan memoria.
2. Generar y mallar un chunk se hace en hilos aparte: unos 50 a 70 µs cada paso (medido).
3. Una malla nueva solo cuesta copiar unos 2 KiB a un buffer compartido y 64 B a una tabla: 2 a
   5 µs por chunk (medido). 100 chunks en un frame cuestan 0.5 a 1 ms (estimado).
4. En Bevy cada malla nueva se convertía, se copiaba y se preparaba por vista dentro del frame,
   sin tope. La parte medida cuesta 5 a 20 µs por malla de 16³ (supuesto: 50 a 250 quads). Si
   cientos de mallas caen en un mismo frame, son 1 a 13 ms: el juego baja de 92 a unos 40 a 80
   fps, no a 20. Para llegar a 20 fps tuvo que sumarse algo que no se midió (estimado: el juego
   se borró; sección 6.4). boxel no debería caer por la subida de mallas: no tiene ese trabajo
   extra.
5. Lo que falta resolver o probar:
   - Al cruzar un borde de chunk, también al subir o bajar, boxel recalcula de cero qué chunks
     quiere. A radio 32 tarda 5 a 8.5 ms (medido): entre el 70 % y más de un frame entero a
     144 Hz (6.94 ms). Es el pico más grande hoy.
   - La carga va atada a los frames. Con el bucle a 144 Hz, radio 32 tarda 40 s en cargar.
   - El mundo tiene 6 chunks de alto (y de -64 a 127). No es infinito en vertical.
   - Nada se probó todavía con un juego real: jugador moviéndose, ediciones y cuevas.
6. Escala: los 32 chunks de 16³ del juego en Bevy son 512 vóxeles, o sea radio 16 en boxel.
7. Llamadas a la GPU: boxel nunca dibuja un vóxel suelto. La unidad es el chunk, mallado en quads
   grandes. En el tier High todos los chunks visibles salen en **2 draw calls** (sólidos y hojas):
   el overlay F3 muestra "draw calls 2" con 1096 chunks (medido). En Mid y Low es una llamada por
   grupo de caras visible: 650 con esos mismos chunks (medido).

## Cómo leer este documento

- **Referencias**: `crate/archivo.rs:línea` es `crates/boxel_<crate>/src/archivo.rs`. Por
  ejemplo, `world/streaming.rs:261` es `crates/boxel_world/src/streaming.rs`, línea 261. Los
  ejemplos van como `examples/archivo.rs` y el shader como `shaders/cull.wgsl` (dentro de
  `crates/boxel_shaders`).
- **Números**: "medido" es una medida real de los análisis. "Estimado" es una cuenta hecha con
  medidas. "Supuesto" es un valor que no se midió; va marcado así.
- **Ruido**: mientras se midió, la carga del sistema (load average: cuántos procesos querían CPU
  a la vez, en una máquina de 16 hilos) varió entre 2 y 22 por otros procesos. Los números
  valen como orden de magnitud (sección 11).
- **Escala**:

| | Juego en Bevy | boxel |
|---|---|---|
| Lado de un chunk | 16 vóxeles | 32 vóxeles (`voxel/coords.rs:5`) |
| 32 chunks del juego (512 vóxeles) | radio 32 | radio 16 |
| Radio 32 de boxel (1024 vóxeles) | radio 64 | radio 32 |

La meta "32 chunks a 144 fps" es radio 16 en boxel. Radio 32 de boxel es el doble de distancia
y cuatro veces el área. Este documento mide los dos.

**Palabras que se repiten:**

- **Quad**: un rectángulo de la malla. Una cara de bloque, o varias iguales unidas en una.
- **Greedy meshing**: el mallado que une caras vecinas iguales en un solo quad grande.
- **Slot**: el lugar de un chunk en el mapa del mundo, con el estado de sus datos y su malla.
- **Heap**: la cola con prioridad que decide qué chunk se genera o se mallea primero.
- **Epoch**: un contador por chunk que sube cuando cambia el borde de un vecino. Una malla
  terminada con un epoch viejo se descarta.
- **Arena**: un buffer grande de la GPU donde viven los quads de todos los chunks, cada uno en
  su rango.
- **Buffer temporal (staging)**: memoria intermedia que usa wgpu para copiar datos a la GPU. Un
  "ring de staging" es uno propio que se reusa en vez de crear uno por copia.
- **Multi-draw indirect**: una sola orden de dibujo que la GPU expande en muchas. La lista la
  arma la propia GPU.
- **Pirámide de la cámara** (frustum): el volumen que ve la cámara.
- **Caras de espalda**: las que miran para el lado contrario a la cámara. No se ven.
- **Cutout**: bloques con huecos, como las hojas: cada píxel es opaco o transparente.
- **Histéresis**: un margen extra antes de descargar, para no cargar y descargar el mismo chunk
  al ir y volver por un borde.
- **AO**: oscurecer rincones y esquinas. **Hi-Z**: no dibujar lo que queda tapado por otra cosa.
- **tracy**: un perfilador que muestra en qué se va cada parte del frame.

## 1. La vida de un chunk

```
 HILO PRINCIPAL (schedule Update)                 WORKERS (pool AsyncCompute)
 ================================                 ===========================

 1. Interest: la cámara cambió de chunk
    -> crea el slot (Queued)
    -> lo pone en el heap por distancia
              |
 2. Dispatch: si hay lugar (12 en vuelo) -------> 3. Generar               ~50 us (medido)
                                                     ruido -> Vec de 64 KiB
                                                     -> Paletted comprimido
              +<--------- canal: Generated --------+
 4. Collect: guarda el Arc<ChunkData>
    y avisa a los 26 vecinos
              |
    (espera a que los 26 vecinos tengan datos)
              |
 5. Dispatch del mallado -----------------------> 6. Mallar          51 a 70 us (medido)
                                                     copia 34^3 con bordes
                                                     greedy binario
                                                     -> PackedQuad de 8 B
              +<--------- canal: Meshed -----------+
 7. Collect: chequea version y epoch,
    publica ChunkMeshEvent::Upsert (tope 512 por frame)

 MISMO HILO, schedule Render (después de Last)
 =============================================
 8. Extract: clona el Arc de la malla
 9. Prepare: upsert_chunk                      2 a 5 us (medido)
      - pide un rango en el arena               O(1)
      - write_buffer de los quads               ~2 KiB
      - write_buffer de 64 B en la tabla
              |
 GPU (tier High)
 ===============
 10. compute: un hilo por chunk, frustum y caras de espalda
 11. dos multi-draw indirect (opaco y cutout)
```

| Paso | Hilo | Costo (medido) | Dónde |
|---|---|---|---|
| Generar | worker | 47 a 52 µs | `world/streaming.rs:579-602` |
| Mallar, con lectura de vecinos | worker | 51 a 70 µs | `world/streaming.rs:604-664` |
| Aplicar un resultado de malla | principal | unos 5 µs | `world/streaming.rs:671-706` |
| Subir al render | principal | 2.0 a 2.5 µs (máquina quieta), 3.3 a 4.7 µs (con carga) | `render/renderer.rs:332-369` |
| Borrar del render | principal | 1.2 a 1.8 µs | `render/renderer.rs:371-378` |
| Frame en High con 8000 chunks | principal | 0.03 a 0.05 ms de CPU | `render/path/high.rs:117-152` |

La parte cara (generar y mallar) va fuera del frame. Dentro del frame queda poco por chunk. El
problema real de boxel no está en este camino sino en el paso 1 (sección 7.1).

## 2. Los datos de un chunk

**Coordenadas.** Un chunk mide 32³ (`voxel/coords.rs:5-13`). `ChunkPos` usa `i32` en x, y, z
(`voxel/coords.rs:17`). Dentro del chunk, la posición es un `u16` con z como eje más rápido
(`voxel/coords.rs:125-131`), el mismo orden que usa el mallador, así copia filas enteras.

**Bloques.** Cada estado es un `u16` y el 0 es aire (`voxel/registry.rs:10`). Las propiedades
viven en tablas por campo (`voxel/registry.rs:268`). Hoy hay 12 bloques: las hojas son cutout y
el agua translúcida (`voxel/registry.rs:283`).

**Un chunk** es bloques, luz y un número de versión (`voxel/chunk.rs:9`). Cambiar un bloque sube
la versión (`voxel/chunk.rs:41`). Los bloques se guardan con paleta (`voxel/storage.rs:9-23`):

| Forma | Cuándo | Memoria |
|---|---|---|
| Uniform | un solo estado (aire, piedra) | 0 B |
| Packed de 1, 2, 4 u 8 bits | hasta 2, 4, 16 o 256 estados | 4, 8, 16 o 32 KiB, más la paleta |
| Direct | más de 256 estados | 64 KiB |

La paleta crece sola al editar (`voxel/storage.rs:57-103`) pero nunca encoge: `compact()` existe
(`voxel/storage.rs:173`) y nadie lo llama.

**Luz.** Hoy es uniforme, cielo 15 (`voxel/storage.rs:427-443`), y todos los quads llevan ese
valor fijo (`mesh/packed.rs:64`). El formato de luz por vóxel (32 KiB por chunk) existe pero no
se usa.

**Memoria medida** con el generador Forest (terreno y árboles):

| Radio | Chunks con datos | Memoria de vóxeles |
|---|---|---|
| 12 | 3270 | 13.2 MiB |
| 16 | 5598 | 22.3 MiB |
| 32 | 20 838 | 83.5 MiB (unos 4.1 KiB por chunk) |

**Copias.** Un chunk listo es un `Arc<ChunkData>` (`world/map.rs:22-31`). Un job de malla clona
27 `Arc`: el centro y sus 26 vecinos (`world/streaming.rs:624-632`). Una edición usa
`Arc::make_mut` (`world/streaming.rs:542`): si un job todavía tiene el chunk, se copia entero; si
no, se edita en el lugar.

## 3. El mundo: qué chunks se cargan

**Los chunks no son entidades.** Viven en un `SlotMap` más un `HashMap` de posición a id
(`world/map.rs:125-128`). Un id viejo nunca apunta a otro chunk.

**Estados de un slot** (`world/map.rs:22`, `world/map.rs:35`, `world/map.rs:75-92`):

| Datos | Malla |
|---|---|
| Queued, Generating, Ready, Missing (falló) | None, Queued, InFlight, Ready, Empty |

La malla Ready también queda en CPU, para poder subirla de nuevo si cambia la GPU: 13.4 MiB a
radio 32 (medido).

**Qué chunks se quieren.** Se calcula por columnas xz (`world/interest.rs:27-53`):

- Columnas con malla: las que están a distancia r o menos.
- Columnas con datos: un anillo más, porque el mallado necesita a los vecinos.
- Cada columna va por todo el rango vertical (`world/interest.rs:58-78`).

Esto se recalcula solo cuando la cámara cambia de chunk (también al subir o bajar) o cambia el
radio (`world/streaming.rs:261-270`), pero se recalcula entero (sección 7.1).

**Conteos medidos** (6 chunks de alto):

| Radio | Con datos | Con malla pedida | Mallas no vacías | Quads |
|---|---|---|---|---|
| 12 | 3270 | 2646 | 1096 | 239 515 |
| 16 | 5598 | 4782 | 1951 | 439 764 |
| 32 | 20 838 | 19 254 | 7794 | 1 760 326 |

A radio 32, al cruzar un borde de chunk en x entran unos 402 chunks (67 columnas).

**Vecinos.** Cada slot lleva 26 bits, uno por vecino con datos. Cuando un chunk queda listo,
prende su bit en los 26 vecinos (`world/streaming.rs:487-506`). Un chunk se mallea recién con
los 26 bits prendidos (`world/streaming.rs:40`, `world/streaming.rs:426-441`).

**Descarga.** Un slot se descarga si nadie lo quiere y está más allá de r + 1 + 2 en xz
(`world/plugin.rs:54`, `world/interest.rs:82-87`). Si nunca empezó a generarse, se borra al
momento (`world/streaming.rs:280-287`). Las mallas en ese anillo extra siguen dibujándose hasta
la descarga (`world/streaming.rs:349-360`).

**Resultados viejos.** Una malla terminada solo se acepta si su versión y su epoch siguen
vigentes (`world/streaming.rs:755-805`). Si no, se vuelve a pedir.

**Ediciones.** Se agrupan por chunk, suben la versión y piden remesh del chunk y de los vecinos
de cara tocados, con prioridad máxima (`world/streaming.rs:513-561`). No se remeshean vecinos de
arista ni de esquina (`world/streaming.rs:550-558`). Hoy alcanza porque no hay AO.

## 4. Jobs: generación y mallado

**Orden en cada frame:** Interest, Edits, Dispatch, Collect, en cadena
(`world/plugin.rs:180-203`).

**Cola.** Un heap con la distancia 3D al viewer como prioridad; generación y malla comparten la
cola (`world/scheduler.rs:49-99`, `world/interest.rs:91-97`). Cada vez que el viewer cambia de
chunk se rehace el heap entero (`world/streaming.rs:299-308`). No se prioriza lo que está
delante de la cámara.

**Cuántos a la vez.** El tope por defecto es el doble de los hilos de AsyncCompute
(`world/plugin.rs:210-216`). Con 16 hilos, AsyncCompute tiene 6, así que hay 12 jobs en vuelo
(`app/task_pool.rs:36-86`). Cada job es un spawn propio (`world/jobs.rs:23-28`).

**Generar** (`world/streaming.rs:579-602`). El generador llena 64 KiB y los comprime
(`worldgen/lib.rs:217-277`). Si el chunk queda todo arriba del terreno o todo dentro de la
piedra, sale como Uniform sin recorrer vóxeles (`worldgen/lib.rs:223-238`).

**Mallar** (`world/streaming.rs:604-664`).

- No se lanza job si el chunk es aire uniforme (`world/streaming.rs:426-441`), ni si es opaco
  uniforme con sus 6 vecinos de cara también opacos uniformes (`world/streaming.rs:834`).
- Cada worker reusa unos 210 KiB de memoria de trabajo (`world/streaming.rs:108-122`).
- Copia el chunk y un borde de los vecinos a una grilla de 34³ (`mesh/mesher.rs:193-255`), corre
  el greedy binario (`mesh/mesher.rs:339`) y ordena los quads en 8 grupos
  (`mesh/mesher.rs:342-395`).

**Un quad pesa 8 bytes** (`mesh/packed.rs:6-66`): posición, tamaño, cara, textura, AO y luz. Los
grupos son 6 de caras opacas, 1 cutout y 1 translúcido; el translúcido se sube pero no se dibuja
(`mesh/lib.rs:16-26`). Medido en Forest: unos 226 quads de promedio por malla no vacía (1 760 326
quads en 7794 mallas a radio 32, sección 3; unos 2 KiB), máximo visto 1080.

**Contrato.** Todo job devuelve exactamente un resultado, aunque entre en pánico
(`world/jobs.rs:6-11`, `world/streaming.rs:81-106`, `world/streaming.rs:137-165`).

**Vuelta al hilo principal.** Por un canal sin tope (`world/streaming.rs:227`). Collect lo vacía
con un presupuesto de 4 ms y aplica hasta 512 mallas por frame (`world/plugin.rs:52-53`,
`world/streaming.rs:671-706`). Los borrados por descarga no tienen tope.

## 5. Render

**Dónde corre.** En el mismo hilo principal, después de Last, sin paralelismo
(`render/plugin.rs:208-259`).

**Extract** convierte los eventos en operaciones y solo clona el `Arc` de la malla
(`render/plugin.rs:920-982`). Si hubo cambio de GPU, pérdida de dispositivo o cambio de tier,
mete todas las mallas en un solo frame (`render/plugin.rs:952-963`).

**Prepare** sube todo lo del frame sin presupuesto propio (`render/plugin.rs:987-1024`). Por
cada chunk nuevo (`render/renderer.rs:332-369`):

1. Busca un lugar en la tabla de chunks (`render/chunk_table.rs:36-98`).
2. Pide un rango en el arena, O(1) (`render/arena.rs:92-101`).
3. Copia los quads con `write_buffer` (`render/arena.rs:140-147`).
4. Escribe 64 B en la tabla de la GPU (`render/renderer.rs:459-467`), solo en High y Mid.

En wgpu-core 30.0.1 cada `write_buffer` crea su propio buffer temporal (`queue.rs:662` de
wgpu-core). Son dos por chunk nuevo.

**Recursos fijos:**

| Recurso | Tamaño | Dónde |
|---|---|---|
| Arena de quads | 8 Mi quads, 64 MiB; a radio 32 se usa el 21 % (medido) | `render/renderer.rs:40` |
| Tabla de chunks | 32 768 slots, 2 MiB en GPU | `render/renderer.rs:41`, `render/renderer.rs:229-236` |
| Índices compartidos | 98 304 quads, 2.36 MB | `render/path/mod.rs:268-278` |

Nada de esto crece. Si se llena, el chunk no se dibuja y se cuenta como descartado
(`render/renderer.rs:343-353`).

**Tiers:**

| Tier | Cómo descarta chunks | Costo de CPU |
|---|---|---|
| High | compute en GPU, un hilo por chunk (`render/path/high.rs:117-129`, `shaders/cull.wgsl:42-76`) y dos multi-draw indirect (`render/path/high.rs:131-152`) | no depende de cuántos chunks haya: 0.03 a 0.05 ms con 8000 (medido) |
| Mid | recorre todos los chunks en CPU (`render/path/mod.rs:361-394`, `render/path/mid.rs:49-99`) | 0.34 ms con 8000 (medido) |
| Low | igual que Mid, un draw por grupo (`render/path/low.rs:25-60`) | sin medir |

En esa misma prueba de 8000 chunks (`upload_bench`, con los chunks en un disco artificial), la
GPU tardó 0.39 ms por frame (medido).

No hay descarte por oclusión todavía. Solo se descarta lo que está fuera de la pirámide de la
cámara, que llega hasta radio × 32 vóxeles (`examples/voxel_world.rs:505-507`), y los grupos de
caras que miran para atrás (`shaders/cull.wgsl:53`, `render/path/mod.rs:374`).

**Medido en estático a radio 32** (`voxel_world`, 7794 mallas, 1.76 M quads): una vuelta entera
del bucle (mundo y render) tarda unos 0.25 ms de CPU. Sale de las unas 4000 updates/s medidas
(sección 10).

## 6. Comparación con Bevy

### 6.1 Qué hacía Bevy 0.19 con cada chunk nuevo

El hilo aparte solo sacaba del frame el ruido y el mallado. Todo lo demás se pagaba en el frame.

**Tamaño de malla (supuesto).** Las pruebas usan mallas sintéticas con el layout usual:
posición, normal y UV en f32 e índices u32, 152 B por quad. Un chunk real de 16³ con greedy
meshing probablemente tenga entre 50 y 250 quads: las mallas de 32³ de boxel promedian unos 226
(sección 3) y un chunk de 16³ tiene un cuarto de esa superficie. Por eso se midió con 50, 100,
250 y 1000 quads.

1. **Mundo principal.** El chunk es una entidad con malla, material y transform. Cambia de
   arquetipo otra vez cuando se le agrega la caja (`bevy_camera visibility/mod.rs:557-582`). La
   caja se calcula recorriendo todos sus vértices: 0.3 a 1.5 µs con 50 a 250 quads (medido). En
   cada frame con mallas nuevas, varios sistemas recorren todas las mallas del mundo, no solo
   las nuevas.
2. **Extract.** El hilo principal espera al de render. Con la opción por defecto de
   `RenderAssetUsages`, la malla se clona entera: 0.15 a 0.65 µs con 50 a 250 quads, 13 µs con
   1000 (medido).
3. **Hilo de render, subida.** Un bucle en serie sobre todas las mallas del frame
   (`bevy_render mesh/allocator.rs:362-430`). Por cada malla crea un buffer temporal y pasa los
   vértices de un formato a otro, uno por uno. Medido por malla: unos 5 µs con 50 quads, 7.5 µs
   con 100, 17.5 µs con 250 y 57 µs con 1000. Son unos 2 a 3 µs fijos (el buffer temporal) más
   unos 5 µs cada 100 quads (estimado de esas medidas). Si el buffer grande se llena, crea otro
   1.5 veces más grande y copia todo.
4. **El tope por frame no sirve.** `RenderAssetBytesPerFrame` frena otro paso, no esta copia.
5. **Preparación por vista.** Cada chunk se prepara para cada vista activa: la cámara y, si el
   juego tenía prepass y sombras, el prepass y las 4 cascadas de la luz direccional. Las sombras
   de esa luz vienen apagadas por defecto (`bevy_light directional_light.rs:152`) y el prepass
   hay que activarlo.

Ni el costo por entidad (crearla, cambiarla de arquetipo) ni la preparación por vista se
midieron. Son costos fijos por chunk que se suman a los de arriba.

### 6.2 Llamadas a la GPU

En Bevy, cada chunk es una entidad con su malla y se prepara y se dibuja por separado (salvo lo
que Bevy logre agrupar). Con miles de chunks eso son miles de llamadas y miles de preparaciones
por frame, y no hay forma de cambiarlo sin tocar el motor.

En boxel:

| Tier | Llamadas por frame | Con 1096 chunks (overlay F3, medido) |
|---|---|---|
| High | 2, siempre (multi-draw indirect, la GPU decide qué se ve) | 2 |
| Mid y Low | 1 por grupo de caras visible | 650 |

El número del tier High no crece con el mundo. Es lo que permite pensar en radio 32 o más.

### 6.3 Lado a lado

| | Bevy 0.19 | boxel |
|---|---|---|
| Un chunk es | entidad, asset Mesh y RenderMesh | un slot en un mapa y un rango en un arena |
| Bytes por quad | 152 B con el layout usual | 8 B (`mesh/packed.rs:6-66`) |
| Subida | buffer temporal y conversión por malla, en serie | copia de quads ya empaquetados (`render/arena.rs:140-147`) |
| Tope por frame | ninguno efectivo en la copia | 512 mallas por frame (`world/plugin.rs:53`) |
| Trabajo por vista | por cada vista activa: cámara y, si estaban activos, prepass y 4 cascadas | ninguno en CPU; descarte en GPU (High) |
| Costo por chunk en el frame | 5 a 20 µs medidos con 50 a 250 quads, más entidad y vista sin medir | 2 a 5 µs de render más unos 5 µs de mundo, con unos 226 quads (medido) |
| Buffers que crecen | sí, con copia completa | no, tamaño fijo (`render/renderer.rs:40`) |

Medido en esta máquina con 200 mallas de 1000 quads (29 MiB): el camino de Bevy tarda 11.6 ms de
CPU. Copiar esos mismos bytes ya empaquetados con un solo `write_buffer` tarda 2.3 ms.

Crecer el buffer de Bevy, medido: de 128 a 192 MiB, 2.2 ms de CPU más 1.6 ms de GPU; de 256 a
384 MiB, 4.1 ms más 3.2 ms.

### 6.4 Por qué caía a 20 fps (estimación)

| Paso | Valor | Tipo |
|---|---|---|
| Columnas nuevas al cruzar un borde a 32 chunks | unas 65 | cuenta (2 × 32 + 1) |
| Chunks con geometría por columna (mundo infinito en Y) | 2 a 5, o sea 130 a 325 mallas | supuesto |
| Remesh de los vecinos que ganan un vecino | otro tanto: 260 a 650 subidas en total | supuesto |
| Todas las subidas caen en el mismo frame | sí | supuesto |
| Quads por malla de 16³ | 50 a 250 | supuesto (6.1) |
| Costo de la parte medida por subida (caja, clonar, convertir y copiar) | 5 a 20 µs | medido |
| Extra por frame | 1.3 a 13 ms | estimado |
| Frame base a 92 fps | 10.9 ms | dato tuyo |
| Frame resultante | 12 a 24 ms, unos 40 a 80 fps | estimado |

Con mallas de tamaño realista, lo medido baja de 92 a unos 40 a 80 fps, no a 20. Solo con
mallas de 1000 quads (unos 76 µs cada una, sumando lo medido) la cuenta da 20 a 50 ms extra, o
sea unos 20 fps. Y con la generación en otro hilo, lo normal es que las mallas lleguen
repartidas en varios frames, así que el golpe por frame sería menor todavía.

Para llegar a 20 fps (50 ms por frame) faltan unos 25 a 40 ms que lo medido no explica. No hay
perfil del juego, así que no se sabe de dónde salen. Candidatos:

- El costo fijo por chunk que no se midió: crear la entidad, cambiarla de arquetipo y
  prepararla para cada vista (más si había sombras).
- Generar en `ComputeTaskPool` o con `block_on`: el frame termina ejecutando la generación.
- Mallar en el hilo principal.
- Un material por chunk: rompe el batching y rehace bind groups.
- Colliders de malla armados en el hilo principal.
- Un lock compartido entre generadores y el hilo principal.
- Entidades para chunks vacíos, que inflan todos los recorridos.

### 6.5 Qué se lleva boxel de esto

boxel ya evita lo principal: sin entidades por chunk, sin conversión de formato, sin trabajo por
vista en CPU, con quads 19 veces más chicos y con topes por frame. Quedan tres parecidos:

1. Dos buffers temporales de wgpu por chunk (`render/arena.rs:142`,
   `render/renderer.rs:461`). Es el mismo mecanismo que en Bevy, aunque con 2 KiB por chunk.
2. Cuando el arena crezca (M1), no copiar cientos de MB dentro de un frame.
3. El remesh de vecinos multiplica el trabajo. En boxel va en workers, pero cada uno es otra
   subida.

La lección más útil: en Bevy nunca se supo qué pesaba más. En boxel hay que medir el frame
entero mientras se carga, no solo la subida (sección 10, paso 1).

## 7. Puntos débiles de boxel

| # | Problema | Peor caso medido | Dónde |
|---|---|---|---|
| 1 | Recalcular el interés entero al cruzar un borde, también vertical | 6.3 a 8.5 ms en un frame a radio 32 | `world/streaming.rs:261-309` |
| 2 | Carga atada a los frames | 40 s para cargar radio 32 a 144 Hz | `world/streaming.rs:674`, `world/plugin.rs:180-203` |
| 3 | Render sin presupuesto propio | 1.0 a 2.2 ms con 512 chunks en un frame | `render/plugin.rs:987-1024` |
| 4 | Resync de todo en un frame | 16 a 35 ms (estimado) | `render/plugin.rs:952-963` |
| 5 | Costo fijo por frame | 0.24 ms del mundo en reposo a radio 32 | `world/plugin.rs:300-326` |
| 6 | Warning de fixed timestep | sale con un frame de más de unos 140 ms; en tu log, el primero (250 ms o más) | `app/time.rs:39`, `app/time.rs:198-240` |
| 7 | Tope de draws en High | warning desde unos 10 900 chunks vivos | `render/renderer.rs:237`, `render/renderer.rs:428-438` |

### 7.1 Recalcular el interés al cruzar un borde

Cada vez que la cámara pasa a otro chunk, boxel rehace la lista entera de chunks que quiere
(`world/interest.rs:58-78`), recorre todos los slots (`world/streaming.rs:274-291`), crea los
nuevos (`world/streaming.rs:293-297`) y rehace el heap (`world/streaming.rs:299-308`). El costo
depende del tamaño de la vista, no de lo que cambió.

Pasa también al cruzar un borde vertical (cada 32 vóxeles al subir o bajar), aunque no entre ni
salga ningún chunk. La cámara se guarda como chunk 3D (`world/plugin.rs:224`) y cualquier
cambio dispara el recálculo (`world/streaming.rs:267`), aunque la altura no cambie qué chunks se
quieren (`world/interest.rs:68-69`).

| Radio | Costo por cruce (medido) |
|---|---|
| 12 | 1.1 a 1.5 ms |
| 16 | 2.2 a 2.7 ms |
| 32 | 4.8 a 5.5 ms con el bucle libre; 6.3 a 8.5 ms con el bucle a 144 Hz |

El primer cálculo, al arrancar, tarda 16 ms a radio 32. Desglose medido a radio 32: armar la
lista 2.23 ms, recorrer los slots 0.57 ms, buscar en el índice 0.08 ms. El resto de un cruce
(unos 2 a 5 ms) no se midió por partes: crear y borrar slots, rehacer el heap y, quizás, fallos
de caché.

**Arreglo:** calcular solo lo que entra y sale. A radio 32 son unas 67 columnas de cada lado, no
las 3473 de la vista. Cambiar el `BTreeMap` por un set o un bitmap de columnas y no rehacer el
heap entero. Mientras el interés no use la altura, subir o bajar no debería recalcular la lista.

### 7.2 Carga atada a los frames

Dispatch corre antes que Collect, y los lugares en vuelo solo se liberan en Collect
(`world/streaming.rs:674`). Collect corre justo después de Dispatch, cuando el job todavía no
terminó. Su lugar se libera en el Collect del frame siguiente, cuando el Dispatch de ese frame ya
pasó. Resultado: cada lugar hace un job cada 2 frames; con 12 lugares, unos 6 jobs por frame. El
informe M0 ya lo había visto (`docs/informe-m0.md`, M1 punto 1).

| Escenario a 144 Hz | Tiempo de carga (medido) |
|---|---|
| Radio 12, 12 en vuelo | 5.93 s |
| Radio 16, 12 en vuelo | 10.6 s |
| Radio 32, 12 en vuelo | 39.95 s |
| Radio 16, 64 en vuelo | 1.96 s |
| Radio 32, 256 en vuelo | 1.80 s |
| Radio 32, bucle libre | 0.61 s |

Moviéndose a 64 vóxeles/s a radio 32 con 12 en vuelo se juntan 1400 jobs atrasados en 5 s
(medido). Con 256 en vuelo y 256 vóxeles/s no hay atraso (medido). Los workers no son el cuello.

Con el límite de FPS (144 por defecto, ADR 0013) ya se nota. Medido con `voxel_world
--wait-chunks`: radio 12 carga en 6.5 s (sin límite, 0.6 s), radio 16 en 11 s y radio 32 en 41 s
(sin límite, 1.7 s).

**Arreglo:** recolectar antes de despachar, o despachar otra vez después de Collect. Subir el
tope o cambiarlo por un tope de memoria.

### 7.3 Render sin presupuesto propio

El render sube todo lo que llega en el frame; el único freno es el tope de 512 del mundo
(`world/plugin.rs:53`). Los borrados no tienen tope.

| Chunks nuevos en un frame | CPU de subida (medido) |
|---|---|
| unos 90 (streaming con 256 en vuelo) | collect 0.18 ms de promedio, 0.55 ms de máximo |
| 512 | 1.0 a 2.2 ms, más 0.3 a 1.0 ms de GPU |
| 2048 | 4.5 a 10.4 ms |

El piso de unos 2 µs por chunk viene de los dos buffers temporales de wgpu (hipótesis 8.2).

**Arreglo:** presupuesto en bytes o en chunks dentro del render y un solo `write_buffer` por lote
o un ring de staging propio.

### 7.4 Resync de todo en un frame

Con GPU nueva, pérdida de dispositivo o cambio de tier, Extract mete todas las mallas en un frame
(`render/plugin.rs:952-963`). A radio 32 son 7794 chunks por 2 a 4.5 µs: 16 a 35 ms (estimado).
**Arreglo:** subirlas en cuotas.

### 7.5 Costo fijo por frame

`publish_stats` recorre todos los slots y arma un texto en cada frame
(`world/plugin.rs:300-326`, `world/map.rs:176`). El mundo en reposo mide 0.24 ms a radio 32
contra 0.195 ms a radio 12, con el bucle a 144 Hz (medido). En Mid y Low el descarte en CPU
recorre todos los chunks (`render/path/mod.rs:361-394`): 0.34 ms con 8000 (medido).

### 7.6 El warning de fixed timestep

El paso fijo va a 64 Hz, 15.6 ms por paso (`app/time.rs:107-110`). Por frame se corren como
mucho 8 pasos (`app/time.rs:127`); si el tiempo acumulado da para más, el resto se descarta y
sale el warning (`app/time.rs:198-240`). O sea que sale con cualquier frame de más de unos
140 ms.

En tu log fue solo al arrancar: `ran 8 steps this frame, dropped 8 more` son 16 pasos, el tope
de 250 ms al que se recorta el delta (`app/time.rs:39`). Así que el primer frame tardó 250 ms o
más. Elegir la GPU tomó unos 140 ms (tu log: 140.8 ms en la RTX) y probablemente cae en ese
frame. El resto (crear la ventana, el renderer y el primer interés) no se midió por partes. Si
sale en pleno juego, es un pico de verdad.

**Ya corregido** (después de este análisis): el arranque de la GPU se declara como pausa conocida
con `Time::declare_stall` y no se simula; en los primeros frames el aviso baja a debug y, después,
sale como mucho cada 5 s con la cuenta acumulada. Si vuelve a salir en pleno juego, es un pico real.

### 7.7 Otros límites

- **Draws en High.** La lista tiene lugar para 65 535 draws opacos (`render/renderer.rs:237`).
  Pasando unos 10 900 chunks vivos sale un warning y se saltean grupos cualquiera, no los más
  lejanos (`render/renderer.rs:428-438`). Radio 32 tiene 7794 mallas, más las del anillo de
  descarga (`world/streaming.rs:349-360`).
- **Nada crece.** Arena y tabla tienen tamaño fijo; si se llenan, el chunk no se dibuja
  (`render/renderer.rs:343-353`).
- **AO.** Con AO (M1), editar un bloque también cambia vecinos de arista y esquina, que hoy no
  se remeshean (`world/streaming.rs:550-558`).
- **Sin oclusión.** A radio 32 se dibujan 1.76 M quads solo con descarte por pirámide. Hi-Z
  está planeado para M2.

## 8. Hipótesis para medir

| # | Hipótesis | Cómo medirla |
|---|---|---|
| 8.1 | La diferencia entre 4.8 a 5.5 ms y 6.3 a 8.5 ms del interés viene de la frecuencia de CPU y la caché fría después de dormir. | Repetir con la máquina quieta, con tracy, a frecuencia fija. |
| 8.2 | El piso de 2 µs por chunk en la subida es el buffer temporal que crea wgpu en cada `write_buffer`. Está en el código de wgpu-core, no perfilado aparte. | tracy antes de escribir un ring de staging propio. |
| 8.3 | Cada lugar en vuelo hace un job cada 2 frames. Sale del orden de los sistemas y lo confirman los números (unos 5 jobs por frame con tope 12). | Contador de jobs terminados por frame. |
| 8.4 | El frame de GPU de 0.39 ms con 8000 chunks usa un disco artificial, no la escena real. | Medir en `voxel_world` con la cámara volando. |
| 8.5 | 144 fps se sostienen a radio 16 y 32 mientras se carga. | Volar a velocidad fija y medir el peor frame y el percentil 99. Meta: ningún frame de más de 6.94 ms. |
| 8.6 | La caída en Bevy fue sobre todo por la subida de mallas. Con mallas realistas, lo medido explica solo una parte (6.4). | No se puede medir: el juego se borró. Al rehacerlo en boxel, medir el frame entero mientras carga y contar los quads por malla. |
| 8.7 | Un mundo infinito en vertical cabe en memoria y en tiempo de interés. | Medir slots y costo de interés con un radio vertical (sección 9). |

## 9. Mundo infinito en vertical

**Hoy no lo es.**

- El rango vertical es fijo: lo da el generador (`world/streaming.rs:195-199`).
  `TerrainGenerator` da -2 a 3: 6 chunks, de y = -64 a 127 (`worldgen/lib.rs:280-284`).
- La altura de la cámara no cuenta para elegir chunks (`world/interest.rs:68-69`), pero cruzar
  un borde vertical igual dispara el recálculo completo de 7.1.
- Un vecino fuera del rango cuenta como listo (`world/streaming.rs:810-824`).
- La descarga mira solo xz (`world/interest.rs:82-87`).
- Cada columna recorre todo el rango (`world/interest.rs:58-78`). Un rango enorme haría explotar
  el costo y la memoria.
- El generador tiene piso y techo fijos (`worldgen/lib.rs:223-238`).

**Lo que ya sirve.**

- `ChunkPos` ya usa `i32` en y (`voxel/coords.rs:17`).
- La prioridad ya es distancia 3D (`world/interest.rs:91-97`).
- Un chunk de puro aire o puro piedra: sus vóxeles no ocupan memoria (`voxel/storage.rs:9-23`),
  se genera sin recorrer vóxeles (`worldgen/lib.rs:223-238`) y no lanza job de malla
  (`world/streaming.rs:426-441`, `world/streaming.rs:834`). Hace falta, porque la mayoría de los
  chunks de un mundo alto son así: a radio 32, 11 460 de 19 254 mallas pedidas salieron vacías
  (medido).

**Lo que hay que cambiar.**

1. **Interés 3D:** un radio vertical alrededor de la cámara, con un anillo de datos uno más
   grande que el de malla, igual que hoy en xz.
2. **Descarga 3D:** histéresis también en y.
3. **Vecinos sin cargar = no listos.** Si no, el borde de arriba y de abajo se mallea con caras
   de más, que después hay que rehacer.
4. **Interés incremental (7.1):** ya hoy subir o bajar dispara el recálculo completo. Con
   interés 3D, además, cada cruce vertical cambiaría la lista de verdad.
5. **Generador 3D:** sin piso ni techo fijos, con cuevas.
6. **Oclusión:** con cuevas, el descarte por pirámide dibuja mucho bajo tierra. Hi-Z (M2) gana
   peso. Es una hipótesis hasta medirlo.

## 10. Plan hacia 32 chunks a 144 fps sin picos

En orden. Cada paso se mide antes y después.

| # | Qué | Qué arregla |
|---|---|---|
| 1 | Overlay de debug y contadores | sin esto no se puede medir lo demás |
| 2 | Interés incremental | el pico de 5 a 8.5 ms al cruzar un borde, también vertical (7.1) |
| 3 | Desatar la carga de los frames | los 40 s de carga y el atraso al moverse (7.2) |
| 4 | Presupuesto y lotes de subida en el render | ráfagas y el piso de 2 µs por chunk (7.3) |
| 5 | Interés 3D | mundo infinito en vertical (9) |
| 6 | Resync en cuotas | el pico de 16 a 35 ms (7.4) |
| 7 | Oclusión (Hi-Z) | quads bajo tierra y detrás de montañas |

**Ojo con los FPS del log.** La línea `fps=` del log cuenta updates de la App, no frames
presentados (`diagnostic/lib.rs:79-95`). Medido antes del límite de FPS: con `--frames` dio 3861
a 4293 "fps", pero se presentaron 978 frames en unos 8 s. El overlay F3 ya muestra las dos cosas
por separado (frames mostrados y updates, ADR 0013).

**Contadores útiles:** chunks cargados y con malla, jobs en vuelo y en cola, mallas y bytes
subidos por frame, llamadas a `write_buffer` por frame, ms de interés y de collect por frame.

## 11. Cómo se midió

- **Máquina:** 16 hilos y RTX 5050 Laptop con Vulkan. Carga del sistema (load average) entre 2
  y 22 durante las medidas. Repetido cuando se pudo.
- **Builds:** release con LTO thin (una optimización al enlazar), fuera del repo. El repo no se
  tocó.
- **Pruebas de boxel**, en `/home/ezequiel/.cache/boxel-agents/report-a/bxbench` (crate aparte
  que depende de boxel por path):
  - `world_stream RADIUS HZ MOVE_EVERY MOVES [THREADS] [MAX_IN_FLIGHT]`: App sin ventana con el
    mundo Forest y sin render. Mide cada frame y cada fase. Carga, 200 frames quietos y luego
    pasos de un chunk en x.
  - `upload_bench [adapter] [high|mid|low]`: el renderer directo con mallas reales. Mide lotes
    de subidas, borrados y el frame estable con 8000 chunks.
  - `interest_cost`: copia el cálculo de interés y mide sus partes.
- **Corridas del repo:** `voxel_world --no-lkg --timeout 90 --frames 30000 --wait-chunks
  --view-distance 32` quedó listo con 20 838 chunks en unos 1.75 s. `headless_world` (radio 4)
  cargó 534 chunks en 1.9 s y 117 frames.
- **Pruebas de Bevy**, en `/home/ezequiel/.cache/boxel-agents/report-b`: `probe` (solo CPU, con
  `bevy_mesh` 0.19.1) y `gpuprobe` (wgpu 29.0.4 en Vulkan, imitando el `MeshAllocator`).
  Usan mallas sintéticas de 1000 quads (148 KiB). Una segunda tanda, con las mismas pruebas y
  solo el tamaño cambiado, midió 50, 100 y 250 quads. El tamaño de malla es un supuesto: un chunk
  real de 16³ probablemente tenga entre 50 y 250 quads. Lo que no se midió (entidades,
  preparación por vista) va aparte.
- **Código de Bevy leído:** 0.19.1 en `~/.cargo/registry`, 0.19.0 y 0.20.0-rc.2 para comparar.
  En 0.20 no cambia nada de lo que causa el problema.
