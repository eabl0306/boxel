# ADR 0049: texturas de entidad en capas de bloque

- Estado: aceptado (el dueño aprobó EW-BER parte a, con `pack_textures`, para el programa
  "todos los bloques", 2026-10-31)
- Fecha: 2026-10-31

## Contexto

Minecraft dibuja cofres, cajas de shulker, cabezas, la vasija decorada y las camas como entidades
de bloque: un modelo de cajas en código con una textura de entidad (`textures/entity/chest/normal`),
donde cada cara de cada caja es un rectángulo de esa textura. El motor solo leía texturas de bloque
para las capas, y una capa venía de una sola textura del pack (con sus pasos del ADR 0044).

Las cajas ya muestran una capa por cara (ADR 0045) y la tabla de cajas está casi llena: el juego
usa 861 de sus 900 entradas. Recortar cada cara con su `uv` gasta entradas. Una capa hecha a la
medida de la cara, no.

## Decisión

- `LayerRecipe` tiene `pieces`: una lista de `LayerPiece` (una textura del pack y sus propios
  pasos). La capa es la suma de las piezas, de abajo arriba, sobre fondo transparente. Después de
  los pasos de cada pieza se aplican los de la receta. Las piezas se leen solo si el pack no tiene
  ni las texturas del modelo ni las de `fallback` (o la receta no nombra ninguna): una cama usa las
  texturas de bloque de Whimscape y la de entidad de BlockPixel. Si falta la textura de la primera
  pieza, la capa queda procedural con su aviso, como siempre.
- Una pieza puede nombrar `entity/...`: `pack_textures` lee esas texturas con `GuiPack`, una vez,
  solo las que nombra alguna receta. No se parten como tira animada, sea cual sea su forma (la de
  una calavera mide 64 x 32).
- `TextureStep::Place { rect, grid }` es el inverso de `Crop`: pone la imagen en el rectángulo
  `rect` de una imagen transparente de `grid` unidades por lado, con los texeles que la imagen trae.
  Un recorte de una textura de 64 px puesto en una cuadrícula de 16 da una capa de 16 px (y de
  64 px en un pack cuatro veces más fino).
- Así el juego corta la cara de un cofre de su textura de entidad, la gira o la espeja, y la pone
  justo donde la cara de la caja lee su capa. La cara no recorta nada, así que no gasta entradas
  de la tabla. Varias piezas en una capa juntan en una cara caras de varias cajas de Minecraft (la
  tapa y el cuerpo del cofre en una sola caja).
- El motor no nombra contenido: los rectángulos de cada textura de entidad, la geometría de
  Minecraft y qué capa va en qué cara son del juego.

## Datos que pasan por un tick

- `TickContext::keep_data(pos)`: un tick que cambia un bloque por otro conserva los datos de esa
  celda (`EditQueue::keep_data` en el lote de sus escrituras). Lo pide el cofre de cobre: se oxida
  en un tick aleatorio y es otro bloque, pero guarda sus ítems (ADR 0048 lo preveía).

## Lo que no se hizo

- Partes animadas o en ángulos libres (tapas que se abren, banderas, los 16 giros de una cabeza):
  son la parte b de EW-BER, con el pase de entidades (#40).
- Tinte por pieza: ninguna textura de entidad de estos bloques lo pide.

## Consecuencias

- Las recetas sin piezas se leen igual: mismas capas, mismas imágenes en los tres tiers.
- Una capa de entidad cuesta lo mismo que cualquier otra capa (EW-LAYERS). Las mismas caras de
  cofre en las cuatro orientaciones comparten capa cuando se leen igual.
