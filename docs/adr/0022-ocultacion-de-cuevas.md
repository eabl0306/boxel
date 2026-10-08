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
  chunk con datos los tiene, también los que nunca se mallan. Se calculan aunque
  `WorldSettings::cave_culling` esté apagado (alrededor de 5 % del trabajo de un chunk): así,
  encenderlo en plena partida oculta enseguida, sin esperar a que cada chunk cargado se genere o
  se malle de nuevo.
- Una edición que cambia lo que ocluye borra los enlaces del chunk. Su próximo job de malla los
  vuelve a calcular. Los atajos uniformes (aire, roca encerrada, datos perdidos) no necesitan job.
- El slot guarda solo los 15 bits (`ChunkSlot::face_links`). Tres banderas del slot pasaron a un
  byte para que siga en 112 bytes.
- `ChunkSlot::visibility_links` solo confía en los enlaces de un chunk que el render muestra tal
  como es (`ChunkSlot::shown`): la última malla que se publicó se hizo con los bloques de ahora del
  chunk y con los bordes de ahora de sus 6 vecinos, o el chunk no tiene nada que mostrar (`Empty`).
  Un chunk con bloques que no se muestra así se ve a través (`XRAY`, sección 3): su primera malla
  está pendiente o nadie la quiere, su malla se tiró o se perdió en un resync, su job de malla
  falló, un vecino de cara se descargó, o una edición cambió lo que ocluye en él o en el borde con
  un vecino (hasta que se publica su malla nueva). Un chunk sin cargar o sin generar cuenta como
  abierto: ningún vecino puede tener una malla hecha con bloques que ya no tiene, porque al
  descargarse deja de mostrar a sus vecinos.
- Solo lo que ocluye decide qué caras de los cubos que ocluyen tiene una malla (el mesher tapa una
  cara solo contra un cubo que ocluye). Un chunk que se vuelve a mallar solo por la luz sigue
  mostrándose como es y sus enlaces siguen valiendo.
- Antes un chunk sin malla publicada, o con una malla vieja, contaba como abierto, y no alcanzaba:
  donde faltan sus caras, una línea de vista sigue por la roca de sus vecinos (sus caras hacia él
  tampoco se mallan) y la búsqueda confiaba en los enlaces de esos vecinos. Mientras el mundo
  cargaba, o se publicaba una edición en partes, se ocultaban chunks que se veían.
- El render puede rechazar la malla de un chunk (tabla de chunks o arena llenas). Entonces muestra
  una malla vieja o nada, y donde faltan sus caras la roca de alrededor deja ver lo que hay
  detrás. El render avisa al mundo (`VoxelRenderer::take_refused`, `ChunkVisibility::set_unshown`)
  y la búsqueda ve a través de ese chunk hasta que el render lo muestre otra vez. En el frame
  del rechazo el mundo todavía no lo sabe: si el conjunto de ese frame llegó al chunk, el render
  dibuja sin ocultar nada.
- Tras un resync del render (dispositivo o tier nuevo, subidas perdidas), el render vuelve a subir
  las mallas listas del mapa, que no incluyen las de los chunks a medio remallar. El render lo
  avisa (`ChunkMeshResync`) y el mundo manda la próxima malla de esos chunks aunque sea igual a
  la anterior, y hasta entonces no los muestra. Antes no la mandaba: el chunk quedaba sin dibujar
  y la búsqueda confiaba en sus enlaces. El frame del resync no oculta nada: el mundo se entera
  recién en el siguiente.
- El mapa anota cada cambio de esos enlaces, y el paso nuevo `WorldSystems::Visibility` (después
  de `Collect`) los pasa a `ChunkVisibility`.

### 3. La búsqueda (`boxel_world::visibility`)

Un BFS desde el chunk de la cámara, con las reglas de Minecraft y Sodium:

- Se entra a un chunk por una cara y se sale solo por las caras enlazadas a alguna cara de
  entrada. El chunk de la cámara ve sus 6 vecinos sin mirar sus enlaces.
- Un chunk que el render no muestra como es, o cuya malla rechazó (sección 2), se ve a través
  (`XRAY`): de él se sale por todas las caras que se alejan de la cámara, y lo mismo de todo chunk
  al que se entra desde uno así. Contarlo solo como abierto no alcanza: una línea de vista sigue
  por la roca de sus vecinos, donde no hay caras.
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

La búsqueda corre después de los pasos del mundo con presupuesto (`FrameBudget`, ADR 0017), que
aprenden cuánto tarda el resto del frame. Una búsqueda cada pocos frames llegaba cuando esa
estimación ya había bajado, y el frame terminaba más tarde de lo planeado. Ahora el tiempo de la
búsqueda y el de los bits del render no cuentan en esa estimación, y cada frame deja libre un pico
estimado de un frame que busca (`CullingCost::reserve`).

Tampoco se oculta nada si el ojo no ve desde espacio abierto (`eye_in_open`): si un vóxel a
menos del alcance del plano cercano (`Camera::near_reach`, unos 0.17 vóxeles con la cámara por
defecto) ocluye o no está cargado. La búsqueda supone que toda línea de vista empieza en un vóxel
abierto. Con el ojo dentro de la roca (la cámara de vuelo no choca) o pegado a una pared, el
render muestra lo que hay detrás: las caras entre dos cubos que ocluyen no se mallan y la pared
cercana de una cueva mira hacia el otro lado. Minecraft hace lo mismo con la cámara dentro de un
bloque. El conjunto anterior sigue valiendo cuando el ojo vuelve al espacio abierto, porque
depende del chunk del ojo y no de su vóxel. `VisibilityStats::blocked` cuenta esos frames.

### 6. En el render, en los tres tiers

- El renderer convierte el conjunto en un bit por slot. Lo rearma entero con un conjunto nuevo,
  una tabla vaciada o una ventana de GPU movida; si no, solo mira los slots que entraron o
  salieron desde el frame anterior. Una búsqueda que encuentra los mismos chunks devuelve el
  mismo conjunto, así que no rearma nada. Ese tiempo no entra en el de la búsqueda: sale aparte
  en `RenderStats::occlusion_time` y en el resumen de `stress`.
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

- Con el ojo en una cueva, a radio 16 se dibuja de 1.8 a 4.4 veces menos (44 a 77 % de las caras
  ocultas, 61 % de media en 9 cuevas). En la 780M pasa de unos 300 fps a entre 500 y 800, sin
  frames de más de 6.94 ms quieta o girando; caminando por la cueva quedan unos pocos, de GPU. Las
  primeras corridas bajo tierra llevaban la cámara por dentro de la roca, donde parte de lo oculto
  sí se veía: se midió de nuevo con `stress --cave` (una cámara en espacio abierto de una cueva,
  y el escenario `path` que la recorre). El resumen de `stress` avisa si el ojo pasó por la roca.
- Mientras el mundo carga o se publica una edición en partes, se oculta menos: todo lo que hay
  detrás de un chunk que no se muestra como es se dibuja. Una edición que cambia lo que ocluye
  junto a la cámara apaga casi toda la ocultación hasta que se publica su malla y se busca de
  nuevo (hasta 50 ms después, `TIGHTEN_DELAY`). Medido: con 20 ediciones en 20 s junto a la
  cámara, el promedio de chunks ocultos baja menos de 1 %. Antes cualquier malla nueva abría el
  chunk; ahora una malla nueva solo por la luz no lo abre.
- **La puerta del plan no se cumple en la superficie:** oculta solo 16 a 46 % de las caras
  subterráneas a radio 16 (16 % a radio 32). Es la fuga de los chunks de 32³: en un chunk de
  superficie, el aire del valle y una cueva tocan la misma cara, y el enlace por par de caras las
  junta. Lo siguiente, en la fase 5: subceldas de 16³ para los enlaces, o Hi-Z en High.
- En la superficie, ocultar ahorra poco GPU (9 a 26 % de los quads) y la búsqueda cuesta más (más
  chunks abiertos). Con la reutilización queda en un empate dentro del ruido.
- El mundo guarda un `u16` y un bit más por slot (sin crecer el slot) y una grilla de la caja
  (37 KiB a radio 16).
- Tests: enlaces contra un flood fill vóxel a vóxel; la búsqueda contra fuerza bruta en mundos al
  azar, cerca del origen, en 2^31, 2^40 y el límite del mundo; rayos de vóxeles dentro del
  frustum, en cuevas, en la superficie y lejos del origen, solo cruzan chunks visibles; la
  reutilización nunca pierde un chunk que el render necesita; `gpu_cave_culling` da los mismos
  píxeles con y sin culling en los tres tiers de cada adaptador, en 29 vistas y lejos del origen,
  y no oculta nada en 15 vistas con el ojo dentro de la roca o a 1/64 de una pared (sin la
  guarda, esas vistas pierden píxeles en todos los tiers). Con dos chunks rechazados delante del
  ojo, los píxeles tampoco cambian, ni en el frame del rechazo ni después; confiar en sus enlaces
  o solo abrirlos pierde píxeles. Con mallas que faltan o que son viejas (un chunk con datos y sin
  malla entre el ojo y chunks dibujados, las mallas de una edición publicadas en partes, un mundo
  que se carga con sus jobs en cualquier orden), rayos de vóxeles por lo que las mallas publicadas
  dibujan solo cruzan chunks visibles (`cave_culling_drawn`), y el render da los mismos píxeles
  con las mallas que el mundo publicó, en los tres tiers; sin `XRAY` pierde píxeles (y los perdía
  antes del arreglo).
- Mediciones en `docs/rendimiento/cuevas-fase4-2026-10.md`.
