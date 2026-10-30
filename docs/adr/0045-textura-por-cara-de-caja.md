# ADR 0045: una textura por cara de caja

- Estado: aceptado (el dueño aprobó el trabajo en el mesher para el programa "todos los
  bloques", 2026-10-30)
- Fecha: 2026-10-30

## Contexto

Un modelo de cajas (`BoxModel`) mostraba en cada cara la textura del bloque para esa dirección,
con la parte de la textura que queda bajo la caja. Así salen bien una escalera o una valla, pero
no una vela, un yunque o un caldero: en Minecraft cada cara de cada caja elige su textura y el
recorte (UV) que muestra. Tampoco había forma de no dibujar una cara.

## Decisión

- `BoxModel` tiene `textures`: nada (todas las caras como antes) o, por caja, un
  `BoxFaceTexture` por cara. Cada cara dice su capa (`texture`, o la del bloque), su recorte
  (`uv`: el texel de su esquina de arriba a la izquierda, del tamaño de la cara) y si va oculta
  (`hidden`: no se dibuja y no tapa nada).
- Las texturas van en el `BoxModel`, no en `ModelBox`. La tabla de cajas (1.024 entradas, en el
  uniform de los tres tiers) sigue compartida por todos los modelos. Un recorte es otra entrada:
  la misma caja con otro desplazamiento de textura (`ModelBox::with_face_uv`). El registro las
  cuenta contra el tope.
- Cada quad ya llevaba su capa, así que no cambian el formato empaquetado ni los shaders. El
  mesher guarda la capa de cada cara al armar la forma del modelo; en el camino caliente solo
  suma una comparación por cara.
- Recortes de otro tamaño, girados o espejados no los resuelve el motor: el contenido hornea una
  capa con los pasos de `LayerRecipe` (ADR 0044) y la recorta.
- Giros: `ModelBox::turned_x` y `turned_z` (además de `turned`, en Y), `Face::turned_x`, `turned_y`
  y `turned_z`, y `BoxModel::turned`, `turned_x` y `turned_z`, que giran cajas, contorno y
  texturas de cara. Un cuarto de giro en X lleva arriba al norte, como el `x: 90` de Minecraft.
  La textura de una cara girada se ve con la orientación de su nueva cara: si debe girar con la
  caja, el contenido usa una capa girada.
- `pack_textures` elige el modo de alfa de una capa mirando también las caras de caja, y el
  registro expone `box_textures` y `drawn_textures`.

## Lo que no se hizo

- Una capa de render por caja (vidrio translúcido del faro): el faro de Minecraft es cutout, así
  que no hizo falta.
- Leer los modelos propios de los packs (cultivos y rieles de Whimscape): pide leer sus JSON de
  modelos, no es barato. Siguen procedurales.

## Consecuencias

- El contenido existente no cambia: mismos quads (`headless_world`), mismas imágenes en los tres
  tiers y los mismos golden.
- El juego puede copiar los modelos de Minecraft caja por caja: velas, yunques, calderos,
  muelas, atriles, cantos de panel y barrotes, pepinos de mar, entre otros.
