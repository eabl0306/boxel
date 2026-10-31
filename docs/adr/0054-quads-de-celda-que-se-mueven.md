# ADR 0054: quads de celda que se mueven

- Estado: aceptado (el dueño aprobó EW-BER parte b, con el render protegido, para el programa
  "todos los bloques", 2026-10-30)
- Fecha: 2026-10-31

## Contexto

EW-BER parte b son las entidades de bloque que se mueven o dependen del mundo: la tapa de un
cofre o de una caja de shulker que se abre, la campana que se balancea, el haz del faro que gira y
sube hasta el cielo, la jaula del canalizador. Lo quieto ya se dibuja: con cajas de modelo (ADR
0049) o con quads de celda horneados en la malla del chunk (ADR 0050 a 0053).

Lo que pesaba:

- **Hornear lo que se mueve.** Un quad de celda horneado cambia solo con un remallado del chunk:
  llega unos frames tarde y cuesta un remallado por paso. Una tapa que se abre en medio segundo
  serían diez remallados con saltos.
- **El pase de modelos de entidades (ADR 0040).** Ya dibuja cajas posadas cada frame, pero con
  una piel por instancia y el sombreado de las entidades (dos luces direccionales). Una tapa
  abierta así no se ve como el cofre cerrado del terreno. Tampoco tiene brillo pleno sin sombreado
  ni textura que se repite, y el haz repite la suya cientos de veces.
- **El tier Low.** Un solo bloque uniforme y un solo par textura/sampler por pipeline. Una
  pipeline nueva tendría que cumplirlo y dar los mismos píxeles en los tres tiers.

El pase de quads de celda ya tiene todo eso: dos arreglos de texturas (el de celdas y el de
bloques), el sombreado del terreno con la luz de la celda, brillo pleno por quad y una pipeline
igual en los tres tiers. Le faltaba aceptar quads que no salen de una malla.

## Decisión

- `MovingCellQuad` (en `boxel_render`): una celda (`BlockPos`) y un `CellQuad` con las esquinas
  en bloques desde la esquina menor de esa celda. Las esquinas pueden salir de la celda: el haz
  sube 2.048 bloques.
- El recurso `MovingCellQuads` del plugin de render es la lista de este frame. El contenido la
  llena cada frame en que algo se mueve. En `Extract`, el plugin le pone a cada quad la luz de su
  celda (`ChunkMap::light_at`, cielo abierto donde no hay luz) y la pasa a
  `VoxelRenderer::set_moving_cell_quads`.
- El pase de quads de celda tiene dos listas más, una por arreglo de texturas. Se rearman y se
  suben solo cuando la lista difiere de la anterior. Una lista igual no sube nada.
- Se dibujan después de los quads de los chunks, con la misma pipeline y el mismo vértice
  (`CellVertex`): el origen del vértice es la celda, como antes lo era el chunk. Sin culling: se
  dibuja todo lo que el contenido da, un draw por arreglo. El contenido da solo lo que puede verse
  (17 quads por cofre abierto, cuatro por tramo de haz).
- El shader no cambia. El sampler repite, así que un quad puede mostrar su imagen varias veces
  (la textura del haz sube 2,5 veces por bloque, como en Minecraft).
- Sin quads que se mueven no cambia nada: no se crea ni se sube nada, y el pase es el de antes.

### En el juego

- Tapas: un contenedor con la tapa no cerrada está en su estado `open` (una propiedad del juego,
  como la del barril), que no dibuja nada como bloque y conserva el contorno y la colisión. Las
  tapas que se mueven se arman con `model_quads` del estado cerrado: las mismas capas y puntos de
  textura, cortadas donde la tapa se junta con el cuerpo, la tapa girada en su bisagra. Las dos
  caras que el cofre cerrado esconde (el interior y el fondo de la tapa) son imágenes de celda
  sacadas de la textura de entidad. Abrir y cerrar cuestan un remallado cada uno; el movimiento,
  ninguno. Tiempos de Minecraft: 0,1 por tick, la curva `1 - (1 - o)^3` del cofre, la subida y el
  giro de 270 grados de la caja de shulker.
- Campana, canalizador: igual, con los estados `ringing` y `active`.
- Faro: el haz entero son quads que se mueven (gira y su textura sube cada frame).
- Estandartes: no se mueven aquí. Van horneados como los carteles (ADR 0051), en la mitad de su
  vaivén.

## Medidas

- `gpu_cells`: los quads de la fila gemela dibujados como quads que se mueven, sin ningún chunk,
  dan los mismos píxeles que horneados en el chunk: 0 % de diferencia en High, Mid y Low (780M,
  RTX 5050 y llvmpipe en Vulkan, Low en GL de la RTX). Pedir la misma lista otra vez no sube nada.
- `headless_world --view-distance 8`: los mismos quads que en main (el mesher no cambió).
- `cells/moving/N` y `cells/still/N` (criterion, la parte de CPU de un frame, una corrida): 30
  quads que cambian, 1,26 µs; los mismos 30 otra vez, 0,30 µs (solo se comparan); 1.000 quads,
  43,3 µs y 10,4 µs.
- Juego: un cofre abierto son 17 quads; armar los de 32 cofres en un frame, 26,6 µs. Abrir o
  cerrar remalla el chunk una vez (un chunk con 32 cofres: 73,5 µs cerrados, 69,5 µs abiertos).
- `just perf-smoke` (sin quads que se mueven): superficie 1.926 a 1.972 fps (+2,4 %, ruido de la
  superficie), bosque 3.849 a 3.848 (0,0 %), p99 igual. Ok.

## Lo que no se hizo

- Translúcidos: el pase hace prueba de alfa y no mezcla. El brillo exterior del haz de Minecraft
  (un cuadrado translúcido a un octavo de su color) no se dibuja. Cuando el pase mezcle (pedido del
  dueño para los bloques translúcidos en marcos) se puede agregar.
- Culling por quad: el contenido da pocos. Si un día son miles, conviene agrupar por chunk como
  los horneados.
- El vaivén de los estandartes: serían quads que se mueven por cada estandarte a la vista, cada
  frame. Se dibujan quietos.

## Consecuencias

- El contenido puede animar cualquier parte de un bloque sin remallar: arma sus quads cada frame
  y el pase los dibuja como a los horneados, con la luz de su celda.
- Un frame con quads que se mueven cuesta su comparación y, si cambiaron, escribir sus vértices y
  subirlos: unos microsegundos para decenas de quads.
