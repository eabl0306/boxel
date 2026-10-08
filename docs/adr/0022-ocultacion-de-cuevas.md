# ADR 0022: ocultación de cuevas (cave culling)

- Estado: aceptado (issue #19, fase 4 del plan de cuevas)
- Fecha: 2026-10-08
- Revisa: el roadmap de `docs/architecture.md` (el cave culling estaba en M2 y solo para Mid y
  Low; llega antes y a los tres tiers)

## Contexto

Bajo tierra, en el overworld a radio 16, la 780M dibuja cerca de 1.1 millones de quads que pasan
el frustum (8.4 millones residentes) y la GPU tarda unos 3 ms por frame: 82 a 105 frames de más de
6.94 ms por corrida (`docs/rendimiento/cuevas-v1-2026-10.md`). Casi todo eso está detrás de roca:
la cámara no lo puede ver. Desde la superficie pasa lo mismo con las cuevas de abajo.

El plan de cuevas (fase 4) pide hacer lo de Minecraft y Sodium: saber qué caras de cada chunk se
ven entre sí a través de vóxeles abiertos, y recorrer los chunks desde la cámara solo por esos
pasos. También pone una puerta: si desde la superficie oculta menos del 70 % de las caras
subterráneas, el siguiente paso son subceldas de 16³ o Hi-Z.

## Decisión

### 1. Enlaces de caras por chunk (`boxel_voxel::ChunkFaceInfo`)

- Dos caras de un chunk están enlazadas si un camino de vóxeles vecinos por cara, todos sin
  ocluir, va de una a la otra dentro del chunk. Ocluye solo un cubo opaco completo
  (`BlockRegistry::occludes`); agua, vidrio, hojas, losas, escaleras, vallas, plantas y aire
  dejan pasar.
- `ChunkFaceInfo { links: u16, opaque_faces: u8, all_opaque: bool }`: los 15 pares de caras
  (`link_bit`), las caras totalmente cerradas y si todo el chunk ocluye.
- Cálculo: un flood fill por bits sobre las 1024 filas de la máscara de oclusión (`u32` en z).
  Uno por región abierta que toca el borde, con semilla en un vóxel del borde que ninguna región
  anterior tocó. Cada región avanza por capas de y con barridos de subida y bajada; dentro de una
  capa, en 2D, rellenando corridas enteras de z con una suma (`fill_runs`). Los chunks uniformes y
  los que no tienen nada que ocluya son de costo constante, y los de 1 bit arman su máscara directo
  de las palabras de la paleta.
- Costo medido: 6 a 7 µs un chunk de cuevas, 11 µs uno de superficie, 6 µs de media en
  profundidad (objetivo del plan: 15 µs).

### 2. Dónde se calculan y cuándo valen

- El job de generación calcula los enlaces junto con el chunk, fuera del hilo principal. Así todo
  chunk con datos los tiene, también los que nunca se mallan.
- Una edición que cambia lo que ocluye borra los enlaces del chunk. Su próximo job de malla los
  vuelve a calcular. Los atajos uniformes (aire, roca encerrada, datos perdidos) no necesitan job.
- El slot guarda solo los 15 bits (`ChunkSlot::face_links`). Tres banderas del slot pasaron a un
  byte para que siga en 112 bytes.
- `ChunkSlot::visibility_links` solo confía en los enlaces cuando la malla del chunk está
  asentada (`Ready` o `Empty`). Un chunk sin cargar, sin generar o con la malla pendiente cuenta
  como abierto. Así nunca se oculta algo detrás de bloques que el render todavía no muestra.
- El mapa anota cada cambio de esos enlaces, y el paso nuevo `WorldSystems::Visibility` (después
  de `Collect`) los pasa a `ChunkVisibility`.

### 3. La búsqueda (`boxel_world::visibility`)

Un BFS desde el chunk de la cámara, con las reglas de Minecraft y Sodium:

- Se entra a un chunk por una cara y se sale solo por las caras enlazadas a alguna cara de
  entrada. El chunk de la cámara ve sus 6 vecinos sin mirar sus enlaces.
- Nunca se vuelve hacia la cámara en un eje: un chunk del lado `+x` de la cámara solo sale por
  `+x`. Es lo que hace la secuencia de chunks de una línea recta. Cada paso aleja un chunk en
  distancia Manhattan, así la cola procesa los chunks por distancia y, al tomar uno, ya conoce
  todas sus caras de entrada.
- Solo recorre chunks que tocan el frustum (una línea de vista queda dentro), salvo los 26 que
  rodean a la cámara. El test es lineal en las coordenadas del chunk y lleva medio vóxel de
  holgura, así nunca rechaza un chunk que el test del render acepta (está probado).
- Cubre una caja del radio de malla del viewer alrededor de la cámara. Un chunk fuera de la caja
  siempre es visible. Un camino que no vuelve sobre ningún eje no puede salir de la caja y
  volver, así que la caja no pierde nada.

Costo medido con la máquina compartida: 22 a 75 µs a radio 16 y 75 a 340 µs a radio 32 con la
caché caliente; en el juego, por búsqueda, 0.12 a 0.2 ms bajo tierra y unos 0.37 ms en la
superficie a radio 16, 0.8 ms en la superficie a radio 32.

### 4. Cuándo se busca

`ChunkVisibility` guarda los enlaces de la caja en una grilla con el mismo orden que la búsqueda.
Al cambiar de chunk copia las filas que siguen dentro y solo lee del mapa la capa que entra. Un
conjunto se reusa mientras nada pueda hacer visible algo más:

- La búsqueda prueba el frustum con 4 vóxeles de margen (`MOVE_SLACK`). El conjunto vale para
  un ojo a menos de esa distancia, en el mismo chunk, mirando igual. Volando recto se busca cada
  pocos vóxeles; al girar se busca enseguida.
- Un enlace que se abre (una edición, un chunk descargado) busca enseguida. Uno que se cierra (una
  malla que se asienta, lo normal mientras el mundo carga) solo puede ocultar más: espera hasta
  50 ms (`TIGHTEN_DELAY`) a la próxima búsqueda.

Volando, se busca en 1 a 4 % de los frames.

### 5. Dónde corre

En el `Extract` del render (`extract_visibility`), con la cámara y el aspecto exactos con que se
dibuja el frame. Si corriera en `Update` usaría la cámara de otro momento del frame, y un chunk
que acaba de quedar a la vista podría aparecer un frame tarde. El radio de la caja es el del
`ChunkViewer` de la cámara, o el del viewer más grande. Sin viewers no se oculta nada.
`WorldSettings::cave_culling` (por defecto encendido) lo apaga.

### 6. En el render, en los tres tiers

- El renderer convierte el conjunto en un bit por slot. Lo rearma entero con un conjunto nuevo,
  una tabla vaciada o una ventana de GPU movida; si no, solo mira los slots que entraron o
  salieron desde el frame anterior.
- High: `cull.wgsl` lee los bits. Van en el mismo buffer que los dos contadores de draws, después
  de ellos (solo los contadores se limpian cada frame), para que el pase siga con 4 storage
  buffers, el mínimo de Vulkan por etapa y el piso de boxel. `View::counts.w` dice si hay bits.
- Mid y Low: el culling en CPU salta los slots ocultos. Low no cambia de shaders ni de límites.
- Los tres: la lista de buckets translúcidos también los salta.
- `RenderStats::occluded_chunks` y `RenderInfo` cuentan los chunks ocultos.
  `boxel_render::cull_counts` cuenta en CPU lo que dibujaría cada tier, con y sin el conjunto.

## Alternativas descartadas

- **Buscar sin frustum** y dejar el frustum al render: se buscaría solo al cambiar de chunk,
  pero oculta mucho menos bajo tierra (75 contra 58 % de las caras a y = -1000, 96 contra 64 % a
  -2280).
- **Calcular los enlaces en cada job de malla:** fácil, pero suma su costo a cada malla. Solo se
  hace después de una edición.
- **Un binding nuevo para los bits en `cull.wgsl`:** serían 5 storage buffers, más que el mínimo
  de Vulkan; un dispositivo con 4 perdería el tier High.
- **Grilla circular con lados potencia de dos:** era 7 veces más grande que la caja y se enfriaba
  en la caché entre frames; la búsqueda en el juego tardaba el triple que en el bench.

## Consecuencias

- Bajo tierra a radio 16 se dibuja de 1.6 a 25 veces menos (38 a 96 % de las caras ocultas, 62 %
  de media). En la 780M, vuelo 64 bajo tierra: de 270 a unos 2 300 fps, p99 de 9.3 a 3.4 ms y
  0 frames de más de 6.94 ms. La bajada queda en 4 a 6 frames lentos por corrida, de GPU, en
  cavernas grandes que sí se ven.
- **La puerta del plan no se cumple en la superficie:** oculta solo 16 a 46 % de las caras
  subterráneas a radio 16 (16 % a radio 32). Es la fuga de los chunks de 32³: en un chunk de
  superficie, el aire del valle y una cueva tocan la misma cara, y el enlace por par de caras las
  junta. Lo siguiente, en la fase 5: subceldas de 16³ para los enlaces, o Hi-Z en High.
- En la superficie, ocultar ahorra poco GPU (9 a 26 % de los quads) y la búsqueda cuesta más (más
  chunks abiertos). Con la reutilización queda en un empate dentro del ruido.
- El mundo guarda un `u16` más por slot (sin crecer el slot) y una grilla de la caja (37 KiB a
  radio 16).
- Tests: enlaces contra un flood fill vóxel a vóxel; la búsqueda contra fuerza bruta en mundos al
  azar, cerca del origen, en 2^31, 2^40 y el límite del mundo; rayos de vóxeles dentro del
  frustum, en cuevas, en la superficie y lejos del origen, solo cruzan chunks visibles; la
  reutilización nunca pierde un chunk que el render necesita; `gpu_cave_culling` da los mismos
  píxeles con y sin culling en los tres tiers de cada adaptador, en 29 vistas y lejos del origen.
- Mediciones en `docs/rendimiento/cuevas-fase4-2026-10.md`.
