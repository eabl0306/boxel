# ADR 0055: quads de celda translúcidos

- Estado: aceptado (el dueño pidió los bloques translúcidos en marcos y estantes: "hagamoslos
  traslucidos", 2026-10-31)
- Fecha: 2026-10-31

## Contexto

El pase de quads de celda (ADR 0050, 0053 y 0054) hace prueba de alfa: un texel se ve entero o no
se ve. Tres cosas del juego necesitan mezclar:

- Un vidrio teñido, el hielo o la miel en un marco o un estante se ven como un bloque
  cutout, sin transparencia.
- El brillo exterior del haz del faro (la segunda capa de `BeaconRenderer`, a un octavo de su
  color) no se dibujaba.
- Los patrones de estandarte con alfa parcial (degradados, bordes al 75 %) se veían con tramado
  o sólidos.

Lo que pesaba:

- **El tier Low.** Un solo bloque uniforme y un solo par textura/sampler por pipeline, el mismo
  shader en los tres tiers y los mismos píxeles.
- **El orden.** Mezclar pide dibujar de atrás hacia adelante, entre los quads translúcidos de
  celda y contra el pase translúcido del terreno (agua, vidrio), que no escribe profundidad.
- **La niebla y las nubes.** Leen la profundidad del pase principal. Lo que no escribe
  profundidad queda debajo de las nubes cuando detrás está el cielo (pasa hoy con el agua y el
  vidrio del terreno, apuntado en el Log del programa).

## Decisión

### El quad

- `CellQuad::blend` (`CellBlend`): `Cutout` (como antes), `Translucent(opacidad)` o
  `Glow(opacidad)`. Un quad translúcido se mezcla por el alfa de su imagen por `opacidad / 255`
  (255 deja el alfa de la imagen) y escribe profundidad, como los ítems translúcidos de
  Minecraft. Un brillo (`Glow`) se mezcla igual pero no escribe profundidad: no tapa nada de lo
  que tiene detrás, como el haz del faro de Minecraft (su capa exterior usa 32).
- `model_quads` da quads translúcidos para un estado del bucket translúcido: un vidrio en un marco
  se mezcla sin que la regla del contenido haga nada.
- Un quad translúcido se ve solo del lado al que mira su `face` (Minecraft descarta la cara de
  atrás de un ítem translúcido), o de los dos si esa cara está a lo largo del quad (un plano
  sombreado como `PosY` de canto). Los texeles con alfa de imagen menor a 0,1 no se dibujan, como
  en el shader de ítems de Minecraft.

### El pase

- El mismo `cell.wgsl` tiene un segundo fragmento, `fs_cell_translucent`: sombrea igual y da el
  alfa de la imagen por la opacidad (en el byte alto de `CellVertex::data[1]`, 0 en un quad
  cutout). Su pipeline mezcla (alfa estándar) y escribe profundidad; una tercera, igual, no la
  escribe, para los brillos. Las tres pipelines tienen un uniform y un par textura/sampler, en
  los tres tiers.
- Los quads translúcidos van en cuatro listas aparte (los de los chunks y los que se mueven, por
  arreglo de texturas). Se dibujan después de los cutout, en el orden de un buffer de índices:
  de atrás hacia adelante por la distancia del ojo al punto más cercano de cada quad (el
  paralelogramo recortado por sus dos bordes), sin los que miran para otro lado. Un quad muy
  largo, como el haz del faro, se ordena bien contra un ítem chico a su lado. Los empates se
  rompen por lista y número de quad, así el orden es siempre el mismo. Los brillos se ordenan
  con los translúcidos; cada tramo seguido de una lista y una clase es un draw.
- El orden se rehace solo en un frame en que cambió el ojo, los chunks dibujados o los quads, y
  escribe solo los índices (24 bytes por quad). Los vértices se suben como antes.
- Sin quads translúcidos no se ordena ni se sube nada, y los quads cutout dan los mismos píxeles
  que antes.

### Dónde se dibujan

Como Minecraft dibuja sus ítems translúcidos (sin el modo Fabulous): después del terreno sólido,
los modelos y los quads de celda cutout, antes del contorno y del pase translúcido del terreno, y
escribiendo profundidad.

- El agua o el vidrio delante de un ítem translúcido lo tiñen, porque se dibujan después.
- El agua o el vidrio del terreno detrás de un ítem translúcido no se ven a través de él (su
  profundidad los tapa). Es el mismo defecto de Minecraft, y se nota poco: un marco va en una
  pared.
- La niebla y las nubes leen esa profundidad: no pintan encima de un ítem translúcido con cielo
  detrás. Para los quads de celda, el problema de las nubes queda resuelto sin costo. Lo que hay
  detrás del ítem y más lejos (una nube) no se ve a través de él.
- Un brillo no escribe profundidad: las nubes pintan encima de él, como en Minecraft. Con
  profundidad, el borde del brillo del faro recortaba las nubes en una franja color cielo (se vio
  en las capturas), por eso es un modo aparte.
- El agua y el vidrio del terreno siguen con el problema de las nubes: arreglarlos pide la
  niebla antes del pase translúcido o una profundidad translúcida para la niebla, trabajo de
  render aparte.

### En el juego

- Marcos y estantes: un bloque translúcido en 3D sale translúcido de `model_quads`. Un ítem plano
  cuya imagen tiene texeles de alfa parcial (un panel de vidrio teñido) se mezcla, guarda su alfa
  en los mips y su relieve toma como lleno todo texel con algo de alfa (`ItemModelGenerator` de
  Minecraft). Los demás siguen con prueba de alfa, que se ve igual.
- Faro: el brillo exterior, un cuadrado de 0,25 que no gira, su textura una vez por bloque, al
  color de cada tramo, un `Glow(32)`. Ocho quads por tramo en vez de cuatro.
- Estandartes: los patrones (no la base) se mezclan por su alfa, sin tramado, y sus imágenes
  guardan el alfa parcial en los mips.

## Medidas

- `headless_world --view-distance 8`: los mismos quads que en main (el mesher no cambió).
- `cells/moving` y `cells/still` (criterion, una corrida por lado, seguidas): 30 quads que
  cambian, 1,27 a 1,40 µs; 1.000, 43,3 a 48,2 µs (+10 %: cada quad elige entre cuatro listas en
  vez de dos); los mismos otra vez, 0,30 a 0,31 µs y 10,2 a 10,6 µs.
- `cells/sort/N` (nuevo, un orden por frame con el ojo moviéndose): 300 quads translúcidos,
  5,2 µs; 3.000, 60 µs.
- `just perf-smoke` (sin quads de celda): superficie 2.116 a 2.132 fps (+0,7 %; la superficie
  varía unos ±10 % de una corrida a otra con cualquier binario), bosque 3.831 a 3.838 (+0,2 %),
  p99 igual o mejor. Ok.
- `gpu_cells`: los quads cutout dan los mismos píxeles que antes (las dos pruebas de siempre
  pasan sin cambios). La prueba nueva, en High, Mid y Low (780M, RTX 5050, llvmpipe y Low en GL
  de la RTX): un rojo a media opacidad es la media del rojo y la pared en luz lineal (tolerancia
  4); un rojo detrás de un azul se ve a través de él con los dos órdenes de la malla, y los dos
  órdenes dan los mismos píxeles; un quad que mira para otro lado no se dibuja; un vidrio chico
  de `model_quads` se dibuja; el vidrio del terreno delante de un quad lo tiñe; un brillo a
  media opacidad es la misma media, y delante del vidrio del terreno lo deja ver (un translúcido
  ahí lo tapa); los mismos quads como quads que se mueven dan los mismos píxeles; y los tres
  tiers dan los mismos píxeles (0 %).
- Juego: 0 capas, 0 imágenes de celda y 0 entradas de cajas nuevas.

## Lo que no se hizo

- El modo Fabulous de Minecraft (cada clase de translúcido en su propio objetivo y una mezcla al
  final): arreglaría el agua detrás de un ítem translúcido, con más objetivos y un pase de
  composición en los tres tiers.
- Orden por quad entre los quads translúcidos de celda y los del terreno: unos van antes y otros
  después, como en Minecraft.
- El orden usa el punto más cercano de cada quad, no un orden topológico como el del terreno (ADR
  0018). Dos quads que se cruzan pueden verse mal; un ítem no tiene caras que se crucen.

## Consecuencias

- El contenido elige por quad si mezcla, con la opacidad que quiera, sin capas ni pipelines
  nuevas.
- Un bloque translúcido del contenido se ve translúcido en un marco sin cambiar su regla.
- Un frame con quads translúcidos cuesta su orden cuando el ojo se mueve: unos microsegundos por
  cientos de quads, y 24 bytes de índices por quad.
