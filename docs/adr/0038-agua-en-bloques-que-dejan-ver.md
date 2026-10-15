# ADR 0038: agua en bloques que dejan ver

- Estado: aceptado. El dueño aprobó el item EW-WATERFACE (#109) el 2026-10-14 y tomó las
  decisiones del plan.
- Fecha: 2026-10-14

## Contexto

Un bloque con agua (ADR 0025) dibuja sus cajas y, aparte, su agua. Hasta ahora el agua no
dibujaba ninguna cara que las cajas del bloque cubrieran enteras en el borde, fuera el bloque que
fuera y estuviera donde estuviera la cara del agua. Fallaba dos veces:

- Las raíces de mangle son una caja entera cutout: su agua perdía las seis caras y se veían
  secas (#109).
- El techo del agua queda entre 11/16 y 16/16. Una placa fina arriba del todo lo tapaba aunque
  no llegara hasta el agua.

Las hojas con agua (#113) llegan después, con su propia decisión.

## Decisión 1: qué caras de su agua tapa el bloque

Como el `LiquidBlockRenderer` de Minecraft:

- Solo un bloque opaco (`RenderShape::Opaque`, el `canOcclude` de Minecraft) tapa su agua. Un
  bloque cutout o translúcido (raíces, hojas, rejas) no tapa ninguna cara.
- Un bloque opaco tapa los costados y el piso que sus cajas cubren enteros en el borde
  (`isFaceOccludedBySelf`): el piso de una losa de abajo, el respaldo de una escalera.
- El techo, solo si queda dentro de la parte sólida de arriba del bloque o sobre ella: cada
  esquina del agua a `top_floor` o más arriba. `top_floor` es la y más baja desde la que las
  cajas llenan enteras todas las capas hasta arriba: 8 en una losa de arriba, 0 en una caja
  entera, ninguna si la capa de arriba no está llena. Minecraft dibuja ese techo, pero no se ve:
  queda dentro de la caja o empata en su plano y pierde (ADR 0025 sec. 6). No dibujarlo deja las
  mallas de hoy.
- El corte del greedy no cambia: el agua sigue sin cara hacia la misma agua o un cubo opaco.

Está en `MaterialTable::held_water_hidden` (`boxel_mesh`), que lee dos bytes por modelo
calculados una vez (`sturdy` y `top_floor`).

## Consecuencias

- Las raíces muestran toda su agua: costados y piso 1/512 hacia adentro y el techo a sus
  alturas. Una hoja cutout en el borde de arriba ya no tapa la superficie.
- Las losas, escaleras, vallas y muros de la demo dan las mismas mallas: los goldens no cambian.
- Un modelo de cajas opaco que Minecraft marca `noOcclusion` (trampillas, andamios, cofres) y
  guarda agua la sigue tapando. Si el juego quiere verla, lo declara cutout.
- Cuesta una lectura de tabla y 4 comparaciones por celda con agua.
- Descartado: tapar el techo solo a 16/16 (suma un quad oculto bajo cada losa de arriba con agua
  y mueve el golden) y mantener la regla vieja para los cutout (las raíces siguen secas).
