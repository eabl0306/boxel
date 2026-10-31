# ADR 0056: quads de patrón fijo en la pantalla

- Estado: aceptado (pedido del dueño para el programa "todos los bloques": "agrega todos los
  bloques, ítems y recetas que hagan falta, incluyendo aquellas cosas que requieren trabajo del
  motor"; el portal del End y el acceso al End entran, el viaje al End no)
- Fecha: 2026-10-31

## Contexto

El portal del End y el acceso al End de Minecraft no tienen textura de bloque. Su
`rendertype_end_portal` dibuja un cielo de estrellas fijo a la pantalla: la textura
`environment/end_sky` estirada sobre toda la pantalla y, encima, 15 capas (portal) o 16 (acceso)
de `entity/end_portal/end_portal`, cada una escalada, girada, corrida y teñida de otro color, y
todo sumado. El punto de la textura sale de la posición del fragmento en la pantalla, no de la
cara: al moverse se ve un fondo que no sigue al bloque.

Lo que pesaba:

- **El tier Low.** Un solo bloque uniforme y un solo par textura/sampler por pipeline, el mismo
  shader en los tres tiers y los mismos píxeles.
- **Las texturas.** `end_portal.png` es de 256 por 256 y `end_sky.png` de 128. Las capas de
  bloque y de celda son del tamaño del pack (16 en los packs del juego): no entran sin perder las
  estrellas. Al juego le quedan 21 capas de bloque y 39 imágenes de celda.
- **El camino caliente.** El sombreado del terreno no debe cambiar para nadie: ni una rama ni
  una textura más.
- **El contenido.** El motor no nombra el portal: el mecanismo tiene que servir para cualquier
  patrón de ese tipo.

## Decisión

### El quad

- `CellImage::Pattern(capas)`: un quad de celda (ADR 0050) cuya imagen es un patrón fijo a la
  pantalla, de un tercer arreglo de texturas, las texturas de patrón que da el contenido.
  `CellQuad::image` es la primera de tres capas seguidas: la base, la imagen de las capas y los
  tintes (fila 0: el texel 0 tiñe la base y el texel `k` la capa `k`). `capas` dice cuántas
  capas van sobre la base, hasta `PATTERN_LAYERS_MAX` (16).
- Se dibuja opaco, a brillo pleno y con la niebla del terreno. No lee `uv`, `full_bright` ni
  `blend`; `tint` multiplica la suma.
- Se ve solo del lado al que mira su `face`, que debe estar a través del quad (una cara de
  cubo), como Minecraft descarta la cara de atrás del portal.

### El shader

- `cell.wgsl` suma `vs_cell_pattern` y `fs_cell_pattern`, la cuenta de
  `rendertype_end_portal.fsh` en sus valores de pantalla (Minecraft no tiene luz lineal): la base
  en el punto de pantalla (`texProj0 / w`: (0, 0) abajo a la izquierda) por su tinte, más cada
  capa `k` escalada por `(4.5 - k / 4) * 2`, girada `(k * k * 4321 + k * 9) * 2` grados, corrida
  `(17 / k, (2 + k / 1.5) * 1.5 * fase)`, a la mitad y corrida un cuarto, por su tinte. Texeles
  más cercanos del mip 0 (Minecraft no filtra ni usa mips), la textura repetida. La suma se
  recorta a 1 como el destino de 8 bits de Minecraft y vuelve a luz lineal.
- Los giros van en una tabla de 16 senos y cosenos de grados enteros (el ángulo tomado módulo
  360 exacto): ningún tier calcula el coseno de un ángulo de miles de radianes. Es un `switch`,
  porque ningún shader guarda un arreglo en una variable local (`naga.rs`).
- Ver solo un lado: el ojo está del mismo lado del plano en las cuatro esquinas (la coordenada
  a lo largo de la normal es el mismo float), así que el vértice de un quad visto de atrás sale
  fuera del volumen de recorte y el quad no dibuja nada. El fragmento no descarta.
- La fase va en `View::light_params.z` (antes 0 y sin leer): los ticks de las texturas animadas
  (`ViewParams::texture_tick`) módulo 24.000 sobre 24.000, el `GameTime` de Minecraft.
  `pattern_phase` lo calcula. No cambia ningún otro shader.

### El pase

- El pase de quads de celda guarda los quads de patrón en una lista más por tipo (los de los
  chunks y los que se mueven) y los dibuja después de los cutout con una cuarta pipeline del
  mismo módulo, con el arreglo de patrones como grupo 0. Sigue siendo un uniform y un par
  textura/sampler por pipeline, el mismo shader en los tres tiers.
- `VoxelRenderer::set_pattern_textures` y el recurso `PatternTextures` del plugin reciben el
  arreglo (`BlockTextureData`, todas las capas de un tamaño, al menos 17 texeles de ancho). Sin
  él no se dibuja ningún quad de patrón. Sin quads de patrón no se crea ni se dibuja nada.
- `RenderStats::cells` suma `patterns` (dibujados) y `pattern_layers`.

### En el juego

- El portal del End dibuja la cara de arriba a 12/16 y la de abajo a 6/16 con 15 capas
  (`TheEndPortalRenderer`); el acceso sus seis caras con 16. Los dos se dibujan desde una marca
  en los datos de su celda, como las estatuas de cobre.
- Las texturas de patrón son tres capas del tamaño mayor de las dos del pack (256 en los dos
  packs): `end_sky` agrandado al vecino más cercano, `end_portal` y los tintes de Minecraft
  guardados en sRGB. Sin pack, unas procedurales. No gastan capas de bloque ni imágenes de celda.

## Medidas

- `headless_world --view-distance 8`: los mismos quads que en main (112.316 en el terreno,
  248.839 en el bosque, 1.290.649 en el overworld). El mesher no cambió.
- `cells/*` (criterion, una corrida por lado, seguidas): 1.000 quads que cambian, 48,7 a 44,3 µs;
  los mismos otra vez, 10,6 a 10,9 µs; el orden de 3.000 translúcidos, 59,5 a 59,9 µs. Ruido: el
  camino de los quads de siempre solo mira una lista más.
- `just perf-smoke surface forest` (sin quads de patrón): superficie 1.909 a 2.140 fps (+12 %; la
  superficie varía unos ±10 % de una corrida a otra con cualquier binario), bosque 3.848 a 3.835
  (-0,3 %), p99 igual o mejor. Ok.
- `gpu_cells`: las tres pruebas de siempre pasan sin cambios (los mismos píxeles). La nueva, en
  High, Mid y Low (780M, RTX 5050, llvmpipe y Low en GL de la RTX): un patrón de imágenes lisas
  da la suma de sus capas teñidas en valores de pantalla con 3 de 255; un patrón de cuadros da
  los mismos píxeles con el quad a dos distancias (fijo a la pantalla) y otros 6.000 ticks
  después (se corre); un quad que mira para otro lado no se dibuja; los mismos quads que se
  mueven dan los mismos píxeles; y los tres tiers dan los mismos píxeles (0 %; GL igual a
  Vulkan en la RTX).
- Juego: 0 capas de bloque, 0 imágenes de celda y 0 cajas para los portales. Las texturas de
  patrón son 3 capas de 256 (unos 1,1 MB con mips).

## Lo que no se hizo

- La cara del acceso que tapa un vecino opaco: Minecraft no la dibuja. Aquí queda detrás de la
  cara del vecino, vista solo desde adentro: no se ve, cuesta su relleno.
- La niebla es la del terreno (lineal por distancia), no la de Minecraft.
- La fase avanza con los ticks enteros, 20 por segundo, no con la fracción del tick: el
  corrimiento por tick es de una décima de texel.

## Consecuencias

- Cualquier contenido puede pedir un patrón fijo a la pantalla de hasta 16 capas con sus tres
  imágenes, sin capas de bloque ni pipelines nuevas.
- El terreno, los quads de celda de siempre y los demás shaders dan los mismos píxeles: solo
  cambia un número del uniform que nadie más lee.
- Un quad de patrón cuesta en el fragmento hasta 17 lecturas de textura y 17 de tinte: pocos
  bloques lo usan.
