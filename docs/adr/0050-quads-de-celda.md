# ADR 0050: quads de celda (ítems dibujados en el mundo)

- Estado: aceptado (el dueño aprobó EW-ITEM y los marcos como bloques para el programa "todos
  los bloques", 2026-10-30)
- Fecha: 2026-10-31

## Contexto

Un marco de ítem es un bloque con pocos estados (6 caras por normal o brillante). El ítem que
sostiene y su giro (8 pasos de 45 grados) son datos de la celda (ADR 0048). El mesher tiene que
hornear ese ítem en la malla del chunk: nada por frame, y un cambio remalla el chunk. Lo mismo
sirve para los estantes y, después, para el texto de los carteles (EW-TEXT).

El problema es dónde viven las imágenes de los ítems:

- El arreglo de texturas de bloques del juego tiene 1.852 capas de 1.900, y el tope de la GPU es
  2.048. Los ítems son unos 1.300 (los que no son bloques y los íconos de los bloques). No caben.
- El tier Low admite un solo bloque uniforme y un solo par textura/sampler por pipeline (wgpu
  no reasigna los bindings en GL 3.3 de NVIDIA). Una segunda textura en las pipelines del
  terreno no funciona en Low.
- En High y Mid, una segunda textura en el terreno pediría muestrear dentro de una rama (WGSL no
  deja `textureSample` en control no uniforme) o muestrear dos veces cada fragmento cutout. Eso
  es costo en el camino caliente para todo el mundo, haya marcos o no.

Opciones que se miraron y se descartaron:

- Capas libres del arreglo de bloques como caché (unas 150 a la vez, con desalojo y remallado):
  un límite de ítems distintos a la vista y un estado compartido frágil entre los hilos del
  mesher y el renderer.
- El atlas de íconos de la GUI: es otra textura (el mismo problema de Low), sin mips y hecho por
  escala de la GUI.
- Varios ítems por capa: pierde resolución o cambia el muestreo de todos los bloques.

## Decisión

Los quads de celda son un pase propio, como los modelos de entidades (ADR 0040): una pipeline
igual en los tres tiers, con un uniform (el `View`), un par textura/sampler (el arreglo de
texturas de celda) y vértices simples. Las imágenes van en su propio arreglo, que da el
contenido.

### Contenido y mesher

- `CellQuad` (en `boxel_voxel`): cuatro esquinas en la celda, el punto de la imagen de cada una,
  la capa de la imagen (`image`), la cara cuyo sombreado toma, si va a brillo pleno y un tinte
  (para el texto de color de E8b). `CellQuad::from_plane` lo arma desde un `ModelPlane`: su caja,
  su tipo, su giro (`spin`, ADR 0046) y sus cuartos de vuelta. El giro de 45 grados del marco se
  calcula en CPU con `ModelPlane::corners`, así que el quad empaquetado y los shaders del terreno
  no cambian ni un bit.
- `CellQuadRule` se registra por bloque (`BlockRegistryBuilder::set_cell_quads`, que también
  marca `MESHES_DATA`). Se ata una vez por estado cuando se arma la `MaterialTable`: recibe el
  registro y el estado (lee la orientación una vez) y devuelve la función que el mesher llama
  con los datos de cada celda.
- `Mesher::mesh_chunk`, después de los bloques, recorre las celdas con datos del chunk central
  solo si algún estado tiene regla y el chunk tiene datos. Cada quad sale movido al chunk y con
  la luz de su celda en `ChunkMesh::cell_quads` (`LitCellQuad`). Los quads de bloque no cambian.
  El hash de la malla incluye los quads de celda solo cuando hay alguno: un chunk sin ellos da
  la misma malla y el mismo hash que antes.
- Un bloque que dibuja quads de celda dibuja también los suyos (el marco, el estante), así que
  una malla con quads de celda nunca está vacía.

### Renderer

- `CellStore` guarda las mallas de los chunks que tienen quads de celda (las mismas `Arc`, sin
  copiar), al subir, quitar o vaciar chunks. `CellPass` arma un buffer de vértices (`CellVertex`,
  40 bytes: origen del chunk en i32, esquina en el chunk, uv, datos) solo cuando un chunk de esos
  cambia. El shader resta el origen de render como el terreno, así que mover la cámara no sube
  nada.
- Cada frame se dibujan los chunks dentro de la ventana de la GPU y del frustum; los rangos
  seguidos se juntan en un draw. Va en el pase principal justo después del terreno sólido, junto
  a los modelos: antes del contorno y del agua.
- El fragmento es el del bucket cutout: prueba de alfa y `shade_fragment` con la luz de la celda
  en las cuatro esquinas, AO abierta y la cara del quad. A brillo pleno, como un bloque emisivo.
- `VoxelRenderer::set_cell_textures` y el recurso `CellTextures` del plugin reciben el arreglo
  (`BlockTextureData`, con mips, hecho como el de bloques). Sin texturas no se dibuja nada. Sin
  quads de celda no se crea nada.
- `RenderStats::cells`: chunks, quads, quads dibujados, draws, subidas y capas.

## Medidas

- `headless_world --view-distance 8`, sin marcos, antes y después: los mismos quads en el
  terreno por defecto (112.316), bosque (248.839) y overworld (1.290.649).
- `gather_mesh` (criterion, una corrida por lado): de +2,7 % a +3,9 % en los cinco casos, también
  en los que el paso nuevo no corre (ninguna regla: una comparación). Parece ruido o layout del
  binario; el perf-smoke no lo ve.
- `just perf-smoke surface forest`: superficie 2.154 a 2.153 fps (-0,1 %), bosque 3.898 a 3.909
  (+0,3 %), p99 igual, ok.
- Imágenes: el juego hace 1.384 (una por ítem) de 16 texeles en 17 ms, unos 2 MB con mips.
- `gpu_cells`: los tres tiers dibujan los mismos píxeles (0 % de diferencia en la 780M, la RTX
  5050 y llvmpipe), también Low sobre GL.

## Lo que no se hizo

- Bloques en 3D dentro del marco: el juego muestra el ícono plano del bloque.
- Mapas dibujados: el marco muestra el ítem del mapa.
- El culling por cuevas no oculta los quads de celda (la profundidad lo hace). Son pocos.
- Quads de celda fuera del chunk de su celda: no hacen falta.

## Consecuencias

- El contenido de siempre da los mismos quads, las mismas imágenes y el mismo costo: el paso del
  mesher no corre sin reglas, y el pase no existe sin quads de celda.
- Un ítem nuevo cuesta una capa del arreglo de celdas (hasta 2.048), no del de bloques.
- El texto de los carteles (E8b) puede usar el mismo camino: glifos como quads de celda con su
  rectángulo de imagen y su tinte.
