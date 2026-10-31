# ADR 0051: carteles dibujados con quads de celda

- Estado: aceptado (el dueño aprobó EW-TEXT para el programa "todos los bloques", 2026-10-30)
- Fecha: 2026-10-31

## Contexto

Los carteles de Minecraft 26.3 son 52 bloques: el cartel, el de pared, el colgante y el colgante
de pared de 13 maderas. Cada uno lleva 4 líneas de texto por lado, con un color de tinte y tinta
luminosa, y el cartel de pie y el colgante giran en 16 ángulos (22,5 grados).

Lo que pesaba en el diseño:

- **Los 16 ángulos.** El quad empaquetado del terreno solo gira en cuartos de vuelta y los planos
  del ADR 0046 en su propio plano. Un tablero girado 22,5 grados alrededor de Y no se puede
  expresar sin cambiar el quad empaquetado, y eso queda fuera de esta tarea.
- **Las capas.** El juego usa 1.854 de sus 1.900 capas de bloque. La textura de un cartel es una
  hoja de 32 por 32 (`block/<madera>_sign` en 26.3) con 1,5 texeles por dieciseisavo: las caras
  del tablero no caben en una capa de 16 sin perder la mitad del detalle, y una capa por cara y
  madera serían cientos de capas.
- **El texto.** Cada cartel tiene texto propio, sin límite de combinaciones: no puede ser una
  imagen por cartel. Tiene que salir de glifos.
- **El costo.** Un chunk sin carteles no puede pagar nada en el camino caliente.

Los quads de celda (ADR 0050) ya resuelven casi todo esto: van en cualquier ángulo (las esquinas
se calculan en CPU), muestran un rectángulo de una imagen del arreglo de texturas de celda (que
el contenido llena y tiene lugar hasta 2.048 imágenes), llevan tinte por quad y brillo pleno, y
solo se calculan para las celdas con datos de los bloques que tienen regla.

## Decisión

El cartel entero se dibuja con quads de celda: el modelo y el texto. El motor no cambia; esta
decisión es del juego y de cómo usa el ADR 0050.

### El modelo

- La regla de quads de celda del cartel (por estado) arma una vez los quads de su modelo: los
  elementos de los modelos de bloque de Minecraft 26.3 (`template_sign_rot_0`,
  `template_wall_sign`, `template_hanging_sign_rot_0`, `template_attached_hanging_sign_rot_0`,
  `template_wall_hanging_sign`), con el giro de cada elemento y después el del estado
  (`rotation` por 22,5 grados, o la dirección de la pared), con las mismas reglas de UV de las
  caras que Minecraft. Así los 16 ángulos salen exactos.
- La textura es la hoja de 32 por 32 del pack (`block/<madera>_sign`). Un pack sin ella (como
  BlockPixel) da su textura de entidad vieja (`entity/signs/<madera>`, 64 por 32) y el juego la
  reordena a la hoja de 26.3 con una tabla de rectángulos. Sin ninguna, la hoja sale de las
  tablas de la madera.
- La hoja va al arreglo de celdas partida en cuatro imágenes de 16 por 16 (sus cuartos), así no
  pierde texeles. Una cara que cruza la mitad de la hoja se parte en dos quads. Son 26 hojas,
  104 imágenes.
- La sombra de cada cara es la de la cara del cubo más cercana a su normal girada. Sin oclusión
  ambiental, como el modelo de Minecraft (`ambientocclusion: false`).

### El texto

- Los glifos son imágenes del arreglo de celdas, una por carácter de un juego fijo (ASCII
  imprimible, Latin-1 y la i sin punto: 192 imágenes). Cada imagen cubre 16 por 16 píxeles de
  fuente con el glifo blanco en su lugar, y el quad muestra solo el rectángulo del glifo.
- La fuente es la de la interfaz del juego (la del pack, si no `font8x8` del motor): las mismas
  medidas con las que la pantalla de edición corta las líneas. Se lee al arrancar, antes de
  mallar ningún chunk.
- La disposición es la de Minecraft (`AbstractSignRenderer` y las transformaciones de
  `StandingSignRenderer` y `HangingSignRenderer`): escala de 1/96 de bloque por píxel de fuente
  (0,9/64 en el colgante), 10 píxeles por línea (9 en el colgante), líneas centradas, el dorso
  girado media vuelta.
- Color: el texto común es el color del tinte por 0,4, iluminado por la luz de la celda. El texto
  luminoso es el color del tinte a brillo pleno sobre un contorno de ocho copias desplazadas un
  píxel en el color oscuro (crema para el negro), un milésimo de bloque detrás.

### Los datos y el ancla

- Lo que guarda un cartel son datos de celda (ADR 0048): el texto de cada lado (las 4 líneas
  separadas por `\n`), el tinte y el brillo de cada lado, y si está encerado.
- La regla de quads de celda solo corre en celdas con datos. Por eso un cartel siempre tiene
  datos: al ponerlo el juego escribe sus datos en blanco en el mismo lote.
- Una malla con quads de celda debe tener un quad de bloque propio (ADR 0050: el streaming y el
  renderer tratan una malla sin quads como vacía). El cartel no dibuja nada como bloque, así que
  su modelo de cajas lleva un ancla: un plano vertical chico por el eje de giro, dentro del
  tablero, en la dirección más cercana al tablero (0, 45, 90 o 135 grados), con la textura de las
  tablas. Desviado a lo sumo 22,5 grados, su borde queda 0,54 treintaidosavos dentro del tablero,
  que tiene 1,33 de medio grosor. Son unas pocas entradas de la tabla de cajas, compartidas por
  todas las maderas.

## Medidas

- Bloques sin carteles: el mesher y el renderer no cambian, así que dan los mismos quads y el
  mismo costo. El paso de quads de celda ya existía (ADR 0050) y solo mira celdas con datos.
- El modelo de un cartel de pie son 15 quads de celda; el texto, uno por glifo, nueve si brilla.
- `just bench signs` del juego (criterion, una corrida): un chunk con 64 carteles de pie en sus
  16 giros sobre un piso de piedra. Solo el piso: 67,5 µs. Con los carteles en blanco: 118,8 µs
  (960 quads de celda). Con 4 líneas en los dos lados y la mitad brillando: 851 µs (15.936 quads
  de celda). Es tiempo de mallado del chunk, que corre al cambiar algo en él, no por frame.
- Imágenes de celda: 1.384 ítems, 192 glifos y 104 cuartos de hoja (1.680 de 2.048).
- En el juego: 0 capas nuevas (1.854 de 1.900) y 7 entradas de la tabla de cajas, las anclas
  (921 a 928 de 1.024).

## Lo que no se hizo

- El contorno del texto luminoso se dibuja siempre. Minecraft lo dibuja solo a menos de 16
  bloques (salvo el negro); esa distancia depende de la cámara y los quads de celda se hornean.
- Caracteres fuera del juego fijo: la pantalla de edición no los acepta.
- Formato de texto (colores por tramo, negrita): Minecraft tampoco lo da al escribir en un
  cartel.

## Consecuencias

- Los carteles no gastan capas de bloque y casi nada de la tabla de cajas.
- Un bloque nuevo de la misma familia (otro tipo de cartel, los estandartes del E9 si se
  dibujan igual) solo agrega hojas al arreglo de celdas.
- El camino sigue abierto a modelos 3D chicos en los quads de celda (E8c): un modelo es una lista
  de quads con su imagen, como el del cartel.
- Si algún día el streaming acepta mallas con solo quads de celda, el ancla se puede quitar.
