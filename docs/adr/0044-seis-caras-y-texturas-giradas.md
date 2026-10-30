# ADR 0044: seis texturas por cubo y capas giradas

- Estado: aceptado (el dueño aprobó el trabajo en `pack_textures` para el programa "todos los
  bloques", 2026-10-30)
- Fecha: 2026-10-30

## Contexto

`FaceTextures` tenía tres texturas: arriba, abajo y una para los cuatro lados. Un horno, una mesa
de trabajo o una calabaza tallada necesitan una cara distinta de las otras. El mesher ya guardaba
seis capas por material, así que solo faltaba el dato.

Además, varios bloques de Minecraft usan la misma textura girada o espejada: el azulejo (glazed
terracotta) gira su dibujo en cada cara, y la puerta con bisagra a la derecha muestra su textura
al revés. El motor no tenía cómo hacer una capa girada a partir de la textura de un pack.

## Decisión

- `FaceTextures` guarda seis capas, una por cara en el orden de `Face::index`. Se construye con
  `all`, `new(arriba, abajo, lado)` (las tres caras de antes) o `from_faces`, y se cambia con
  `with`, `set` y `set_sides`. Los campos `top`, `bottom` y `side` ya no existen; el demo y el
  juego pasaron a los constructores.
- Un cubo girado por `axis` (troncos) sigue igual: arriba y abajo en las caras del eje, la
  textura de +X en las otras cuatro.
- `pack_textures` elige el modo de alfa de cada capa mirando las seis caras (antes solo +Y, -Y y
  +X). Una capa que solo usa el frente de un bloque cutout ahora recibe su alfa binaria.
- `LayerRecipe` tiene `steps`: una lista de `TextureStep` (`Rotate(cuartos)`, `MirrorX`,
  `MirrorY`, `Crop { rect, grid }`). Se aplican en la CPU a cada parte de la capa, después de
  `quarter` y antes de escalar al tamaño del arreglo, así un recorte conserva la resolución del
  pack. También se aplican a cada frame de una textura animada. `apply_steps` es público para que
  el juego pinte sus capas procedurales igual.
- No cambian los shaders ni el mesher. Cada capa girada es una capa más del arreglo (EW-LAYERS).

## Orientación de las caras

El motor muestra la textura así: en ±Y, `u` va por +X y `v` por +Z; en ±X, `u` va por +Z; en ±Z,
`u` va por +X; en los lados `v` baja. Minecraft difiere en tres caras: norte (-Z) y este (+X) se
ven espejadas de izquierda a derecha, y abajo se ve espejada de arriba a abajo. Nadie lo notaba
porque casi todas las texturas de lado son simétricas.

El motor no lo corrige: cambiar el shader movería la imagen de todo bloque existente. El juego
calcula, cara por cara, el giro que hace falta para verse como Minecraft y lo hornea como capa.
Esa cuenta vive en una sola función del juego (`content/faces.rs`), así que si algún día se
corrige el shader, se borra el giro de ahí.

## Consecuencias

- Las imágenes del contenido existente no cambian: los tests golden del registro, del mesher y de
  texturas siguen iguales, en los tres tiers.
- El juego suma B9 (mesas, hornos, calabazas, champiñones gigantes, colmenas y los 16 azulejos) y
  las puertas espejadas donde Minecraft las espeja.
- La extensión "máscara de diferencia" para menas sobre otras rocas no se hizo: no hace falta
  todavía.
