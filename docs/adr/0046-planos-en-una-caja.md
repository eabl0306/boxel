# ADR 0046: planos dentro de una caja

- Estado: aceptado (el dueño aprobó el trabajo en el formato empaquetado y los shaders de los
  tres tiers para el programa "todos los bloques", 2026-10-30)
- Fecha: 2026-10-31

## Contexto

Minecraft dibuja muchos bloques con elementos planos girados 22,5 o 45 grados: la cruz de una
planta en maceta, la mecha de una vela, las cadenas, el asa del farol, los abanicos de coral, los
lados del fuego. El motor solo tenía la cruz de las plantas, siempre de celda entera. El juego
los imitaba con cajas finas y derechas.

Los 8 códigos de forma del quad empaquetado ya estaban usados (el 3 es el muro de borde, ADR
0031), así que el plan de usar el código 3 no servía.

## Decisión

- `BoxModel` tiene `planes`: una lista de `ModelPlane`. Un plano es un quad sobre un plano de una
  caja propia: el plano medio de un eje (`PlaneKind::Mid`) o una de las dos diagonales a lo largo
  de un eje (`Rising`, `Falling`). Una diagonal a lo largo de Y es una cruz pequeña o corrida; a
  lo largo de X o Z es un plano inclinado, con la pendiente que dé la caja.
- La caja va en treintaidosavos (`PLANE_UNIT`), dentro de la celda, para los planos de Minecraft
  que caen en medio texel (la mecha de 7,5 a 8,5).
- La textura es un rectángulo de la capa (`uv`, como el `uv` de una cara de Minecraft), que puede
  recortar, estirar o espejar, más cuartos de giro (`turn`). La capa es la del plano o la del
  lado del bloque. Son las reglas de cara de caja del ADR 0045, con estirado.
- Se dibuja por los dos lados, como la cruz: sin sombreado de cara, sin AO y con la luz de su
  celda. Siempre va al cubo cutout (o translucent), aunque el bloque sea opaco.
- Formato: un plano es un quad `SHAPE_CROSS` de cara +Y, -Y, -X o -Z. Las cruces de siempre usan
  las caras +X y +Z y no cambian ni un bit. La cara dice cuántos cuartos de vuelta gira el plano
  sobre el eje vertical de la celda (`quarter`, 0 a 3): la orientación del bloque. Los bits de
  `w - 1` y `h - 1` llevan el índice de su entrada en la tabla de cajas; el byte de AO lleva el
  tipo de plano (4 bits) y el giro en su plano (4 bits).
- La entrada de la tabla (`PlaneEntry`) guarda la caja y el rectángulo de textura, en las mismas
  dos palabras que una caja. El tipo, el giro y los cuartos de vuelta van en el quad, así que los
  dos planos de una cruz y las cuatro orientaciones de un bloque comparten entrada. Cuenta contra
  el tope de 1.024 como una caja. Con los cuartos en el quad, los planos del juego bajan de 255
  a 143 entradas, y el juego queda en 820 de su presupuesto de 900.
- `ModelPlane::corners` calcula en CPU lo mismo que el shader. `BoxModel::turned`, `turned_x` y
  `turned_z` giran también los planos: buscan el tipo, el giro y el espejo cuyas esquinas son las
  giradas.

## Giro en el plano (para los marcos de E8)

- `spin` gira el quad sobre su centro, en su plano, en dieciseisavos de vuelta (22,5 grados por
  paso, 4 bits). Un marco de ítem gira de 45 en 45: `spin` 0, 2, 4 y así. Los cuartos de vuelta
  son exactos. Ya está en el shader y probado en los tres tiers.
- Lo que le falta a E8: el ítem por celda (EW-BE) y que el mesher hornee su plano con la capa del
  ítem. El ítem plano es un `Mid` con `spin`. El quad girado puede salirse de su caja, pero no de
  la celda.

## Lo que no se hizo

- Planos fuera de la celda. El abanico de coral de Minecraft sobresale del bloque y el fuego mide
  1,4 bloques. Aquí quedan dentro de la celda. Sacarlos pide agrandar las cajas de culling de los
  chunks.
- El contorno de selección por bloque para `BlockModel::Cross`. Las cruces pequeñas usan un
  `BoxModel` con planos, que ya trae su contorno.
- Bloques con contorno vacío no seleccionables: es física (`boxel_physics`), no entraba aquí.

## Consecuencias

- El contenido de siempre da los mismos quads y las mismas imágenes: ningún quad viejo cambia.
- El juego puede copiar casi todos los planos de Minecraft. Cuesta una entrada de la tabla por
  plano distinto (caja y rectángulo), compartida entre bloques.
