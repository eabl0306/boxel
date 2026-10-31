# ADR 0052: el giro de la caja va en el quad

- Estado: aceptado (el dueño aprobó codificar el giro de las cajas en el quad, 2026-10-31)
- Fecha: 2026-10-31

## Contexto

La tabla de cajas de modelo tiene 1.024 entradas (ADR 0020): el índice del quad tiene 10 bits y
la tabla vive en el uniform de la vista, el único bloque uniforme del tier Low. Una caja no tenía
orientación, así que cada copia girada era otra entrada: una escalera mirando a cada lado son
cuatro juegos de cajas, un marco de ítem cinco cajas por seis caras. El juego usaba 921 de las
1.024, casi todas en copias giradas. Los planos ya llevaban su giro en el quad (ADR 0046).

## Decisión

- Una cara de caja (`SHAPE_BOX`) dice cuántos cuartos de vuelta, en el sentido del reloj visto
  desde arriba, gira su caja respecto de su entrada, como `ModelBox::turned`. Los cuatro giros
  de una caja comparten entrada.
- Solo giros alrededor de Y. Medido en el juego: con los cuatro giros en Y, sus 770 cajas pasan a
  482 entradas; con los 24 giros del cubo serían 443. Los 39 de diferencia no pagan los 3 bits
  que faltarían (habría que agrandar el quad).
- Los bits. El quad de 64 bits no tiene bits libres, pero una cara de caja deja dos sin usar:
  - El bit 11 de la capa. Una cara de caja muestra una capa de 0 a 2.047
    (`MAX_BOX_FACE_LAYERS`), el tope de capas de las GPU que usamos. El registro rechaza una
    caja o un bloque de cajas que muestre una capa mayor (`RegistryError::Full`).
  - El bit 0 de los flags, que en las otras formas es `FLAG_EMISSIVE`. Una cara de caja a brillo
    pleno no tiene AO ni usa la luz, así que se marca con el valor de cara 6
    (`BOX_BRIGHT_FACE`, libre en las cajas) y guarda su cara en los bits 0 a 2 del byte de AO y
    sus giros en los bits 3 y 4.
  - Así, una cara normal lleva el giro en el bit 0 de los flags (bit bajo) y en el bit 11 de la
    capa (bit alto). Ningún bit de los planos (ADR 0046), de los datos por bloque (ADR 0048) ni
    de los quads de celda (ADR 0050) cambia.
- La entrada de un giro es la menor de las cuatro versiones válidas de la caja (por mínimo,
  máximo y desplazamiento de textura): `ModelBox::canonical` da la entrada y los giros. El mesher
  guarda en la tabla esa entrada. El registro cuenta entradas así contra el mismo tope de 1.024,
  y `BoxModel::table_boxes` y `table_entries` devuelven esas entradas. El contenido no cambia:
  `ModelBox::turned` y `BoxModel::turned`, `turned_x` y `turned_z` siguen igual.
- Los recortes de textura (ADR 0045) son desplazamientos de la caja, así que giran con ella. Un
  recorte comparte entrada con otro giro solo cuando sus desplazamientos son exactamente los de
  la caja girada. No se hace ninguna equivalencia módulo 16: las coordenadas de textura salen
  iguales bit a bit.
- El shader de los tres tiers (`common.wgsl`) lee la entrada como enteros, la gira de a un cuarto
  (`(x, z) -> (16 - z, x)`, el desplazamiento `(-z, y, x)`) y recién entonces pasa a flotantes.
  Las posiciones y las coordenadas de textura son las mismas que con la copia girada, así que la
  imagen es la misma. Todos los quads pagan una comparación y una selección por vértice para la
  cara; las cajas pagan además el giro, hasta tres vueltas de un bucle de enteros.
- En CPU, `PackedQuad::model_box_face` arma la cara de caja y `box_turn`, `face`, `layer`,
  `is_emissive` y `ao_byte` la leen. `quad_model_box` da la caja que dibuja un quad (la entrada
  girada); la usan el ordenador de translúcidos y los tests.

## Medidas

- Juego (`just q budgets`): en game main 754868f, 928 entradas antes y 640 después (quedan 384
  libres de 1.024). En 9787c6f eran 921 (770 cajas y 151 planos) y 633 (482 cajas y los mismos
  planos).
- Contenido de los ejemplos: la tabla pasa de 160 a 56 entradas.
- `headless_world --view-distance 8`, antes y después: los mismos quads en el terreno
  (112.316), el bosque (248.839) y el overworld (1.290.649).
- `mesh/shapes` y `gather_mesh/shapes` (criterion, una corrida por lado, 2.362 caras de caja en
  los dos): 70,97 a 71,74 µs (+0,8 %) y 147,09 a 146,44 µs (-0,3 %, sin cambio).
- `just perf-smoke surface forest`: bosque 3.921 a 3.907 fps (-0,4 %). La superficie es bimodal:
  sale cerca de 1.920 o de 2.150 fps sin importar el binario. Con el árbol como lado nuevo dio
  -10 %; con los mismos dos binarios pasados por ruta dio +11 % y, con los lados cambiados,
  -0,5 %. El binario nuevo midió 2.152 fps las dos veces. Sin empeoramiento.
- `gpu_box_turns`: los quads con giro dibujan los mismos píxeles que las copias giradas en High,
  Mid y Low (0 píxeles distintos en la 780M, la RTX 5050 y llvmpipe), y los tiers coinciden
  entre sí.
- El golden de mallas (`mesh_golden`) cambió una vez a propósito: la tabla de cajas y el hash de
  la malla de la vitrina. Los conteos de quads, buckets y paletas, y las otras mallas, son los
  mismos.

## Lo que no se hizo

- Giros alrededor de X y Z (bloques de seis caras: varas del End, pararrayos, marcos hacia arriba
  o abajo). Siguen siendo entradas propias, como antes. Darían 39 entradas más en el juego y
  piden 3 bits que no hay sin agrandar el quad.
- Compartir recortes que solo coinciden módulo 16: cambiaría las coordenadas de textura.

## Consecuencias

- Una caja nueva cuesta una entrada para sus cuatro orientaciones horizontales.
- Una cara de caja no puede mostrar una capa por encima de 2.047. El arreglo de texturas de la
  GPU ya tenía ese tope en la práctica.
- Agregar giros en X o Z pide otro formato de quad (más bytes por quad) o quitarle bits a otro
  campo.
