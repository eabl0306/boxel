# ADR 0053: bloques dibujados con quads de celda

- Estado: aceptado (el dueño pidió bloques en 3D en marcos y estantes, y quitar el ancla de los
  carteles, 2026-10-31)
- Fecha: 2026-10-31

## Contexto

Un marco de ítem o un estante muestran su ítem con quads de celda (ADR 0050). Un bloque se veía
como su ícono plano. Minecraft dibuja en 3D todo ítem cuyo modelo es un modelo de bloque (cubos,
losas, escaleras, vallas) y plano el resto (flores, puertas).

Lo que pesaba:

- **Las texturas.** Las caras de bloque viven en el arreglo de bloques (1.854 de 1.900 capas en el
  juego). El arreglo de celdas tiene 1.680 de 2.048: no entran copias de las caras de bloque.
- **El tier Low.** Admite un solo bloque uniforme y un solo par textura/sampler por pipeline, y
  los tres tiers deben dar los mismos píxeles.
- **La geometría.** Las caras de caja, cruces y planos se arman en el shader del terreno (ADR
  0020, 0045, 0046, 0052). Un bloque chico en un marco tiene que verse igual que en el mundo.
- **El ancla de los carteles (ADR 0051).** El streaming y el renderer tomaban por vacía una malla
  sin quads de bloque, así que cada cartel llevaba un plano escondido.

## Decisión

### Quads de celda con capas de bloque

- `CellQuad::source` (`CellImage`) dice de qué arreglo es `image`: `Cell` (el de celdas, como
  antes) o `Block` (el de bloques, el mismo que dibuja el terreno, con sus capas animadas).
- El pase de quads de celda guarda los quads en dos listas, una por arreglo, cada una con su
  buffer de vértices. Dibuja las dos con la misma pipeline y el mismo shader; entre una y otra
  solo cambia el grupo 0 (el arreglo de celdas o el de bloques, con el sampler de bloques). Cada
  pipeline sigue con un uniform y un par textura/sampler, y el shader es el mismo en los tres
  tiers.
- El vértice no cambia (`CellVertex`, 40 bytes). El quad empaquetado y los shaders del terreno
  no cambian: los cuartos de vuelta de las cajas (ADR 0052) no se usan aquí.
- `RenderStats::cells` suma `block_quads`.

### El modelo de un estado en CPU

- `model_quads(registry, estado, salida)` da los quads que el estado dibuja en el terreno, solo
  en su celda, como quads de celda de capas de bloque: el gemelo en CPU de `decode_quad` para
  cubos (también los girados, como los troncos), cruces, caras de caja y planos. Mismas esquinas,
  mismos puntos de textura, mismas capas, y deja fuera las mismas caras que el mesher (las que
  tapa otra caja del modelo y las ocultas).
- La regla del contenido los escala, gira y mueve donde quiera. La luz es la de la celda y no hay
  AO, como en todo quad de celda.

### Mallas con solo quads de celda

- `ChunkMesh::is_empty` es verdadero solo sin quads y sin quads de celda. El streaming no cambia
  de código: publica la malla como cualquier otra.
- `VoxelRenderer::upsert_chunk` con una malla sin quads de terreno saca el chunk de la memoria de
  chunks (si estaba) y deja sus quads de celda en el pase de celdas. La reserva de memoria
  (`demand`) no cuenta esas mallas.

### En el juego

- Un ítem de bloque en 3D muestra el modelo del estado cuya variante de Minecraft es el modelo del
  ítem sin girar (escaleras al este, puertas de valla al sur, el resto al norte), con la
  transformación `fixed` del modelo dentro de la de `ItemFrameRenderer`, o `on_shelf` dentro de
  la de `ShelfRenderer`. Gira con los pasos de 45 grados del marco.
- Qué ítems van en 3D sale de los íconos del inventario, corregido donde difieren de Minecraft
  (velas, faroles, calderos y pasteles van planos; azaleas y plantaformas en 3D), revisado
  contra el jar de 26.3 para cada ítem de bloque del juego.
- Los carteles ya no llevan ancla: 7 entradas menos en la tabla de cajas.

## Medidas

- `headless_world --view-distance 8`, sin marcos ni carteles: los mismos quads que en main
  (112.316 en el terreno, 248.839 en el bosque, 1.290.649 en el overworld).
- `gather_mesh` (criterion, una corrida por lado): de -4,8 % a +1,3 % según el caso, en código
  que no cambió. Ruido.
- `just perf-smoke surface forest`: superficie 1.914 a 2.154 fps (la superficie es bimodal, ADR
  0052), bosque 3.854 a 3.854. Sin empeoramiento.
- Juego, `just bench signs` (64 carteles en un chunk): sin el ancla la malla pierde 64 quads de
  bloque; en blanco 119,8 a 118,2 µs, con texto 867 a 863 µs.
- `gpu_cells`: los tres tiers dan los mismos píxeles con quads de capas de bloque (0 % en la
  780M, la RTX 5050 y llvmpipe). Una fila de bloques de la demo (cubo, escaleras giradas, valla,
  antorcha, planta) dibujada con `model_quads` desde celdas sin quads de bloque (una malla de
  solo quads de celda) se ve como el terreno: 0,06 % de píxeles distintos, los bordes que el
  terreno agranda.
- Juego: 0 capas nuevas (1.854 de 1.900), 0 imágenes de celda nuevas (1.680 de 2.048) y 7
  entradas menos de la tabla de cajas (640 a 633 de 1.024).

## Lo que no se hizo

- Translúcidos: el pase de celdas no mezcla, así que un vidrio teñido o el hielo en un marco se
  ven con prueba de alfa, como un bloque cutout.
- Sombreado de ítem de Minecraft (luz direccional de entidad): aquí cada cara toma la sombra de la
  cara del cubo más cercana, como el resto de los quads de celda.
- Los modelos especiales de Minecraft (cofres, cajas de shulker, cabezas, la vasija decorada) se
  dibujan con el modelo de cajas del juego y la transformación del modelo especial.
- El libro del atril sigue como estaba.

## Consecuencias

- Un bloque en un marco o un estante no gasta capas ni entradas de la tabla de cajas.
- Una malla de solo quads de celda se dibuja: un cartel no necesita un quad de bloque propio.
- Si cambia cómo el shader del terreno arma una cara (un recorte, un giro), `model_quads` tiene
  que cambiar igual. `gpu_cells` compara los dos en los tres tiers.
