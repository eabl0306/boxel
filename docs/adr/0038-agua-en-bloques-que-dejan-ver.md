# ADR 0038: agua en bloques que dejan ver

- Estado: aceptado. El dueño aprobó los items EW-WATERFACE (#109) y EW-WETCUBE (#113) el
  2026-10-14 y tomó las decisiones del plan.
- Fecha: 2026-10-14
- Revisa: ADR 0025, sección 4 (qué caras de su agua tapa un bloque con agua) y sección 6 (el
  margen de las caras metidas)

## Contexto

Un bloque con agua (ADR 0025) dibuja sus cajas y, aparte, su agua. Hasta ahora el agua no
dibujaba ninguna cara que las cajas del bloque cubrieran enteras en el borde, fuera el bloque que
fuera y estuviera donde estuviera la cara del agua. Fallaba dos veces:

- Las raíces de mangle son una caja entera cutout: su agua perdía las seis caras y se veían
  secas (#109).
- El techo del agua queda entre 11/16 y 16/16. Una placa fina arriba del todo lo tapaba aunque
  no llegara hasta el agua.

Además las hojas no podían tener agua (#113): un cubo completo nunca la guardaba, y el greedy
no sabe dibujar un cubo cuya celda es agua.

## Decisión 1: qué caras de su agua tapa el bloque

Como el `LiquidBlockRenderer` de Minecraft, salvo en los bloques translúcidos:

- Un bloque cutout (`RenderShape::Cutout`: raíces, hojas, rejas) no tapa ninguna cara de su agua.
  En Minecraft no ocluye (`canOcclude`).
- Un bloque opaco o translúcido tapa los costados y el piso que sus cajas cubren enteros en el
  borde (`isFaceOccludedBySelf`): el piso de una losa de abajo, el respaldo de una escalera.
- Uno translúcido tapa como uno opaco. Si no, una cara translúcida de sus cajas y una cara de su
  agua quedarían en el mismo plano y mirando al mismo lado, y el orden translúcido no las sabe
  ordenar (`boxel_render::sort` no ve el 1/512 y supone que no hay dos caras iguales). Por lo
  mismo el plan ya descartó dos capas translúcidas en una celda.
- El techo, solo si queda dentro de la parte sólida de arriba del bloque o sobre ella: cada
  esquina del agua a `top_floor` o más arriba. `top_floor` es la y más baja desde la que las
  cajas llenan enteras todas las capas hasta arriba: 8 en una losa de arriba, 0 en una caja
  entera, ninguna si la capa de arriba no está llena. Minecraft dibuja ese techo, pero no se ve:
  queda dentro de la caja o empata en su plano y pierde (ADR 0025 sec. 6). No dibujarlo deja las
  mallas de hoy.
- El corte del greedy no cambia: el agua sigue sin cara hacia la misma agua o un cubo opaco.

Está en `MaterialTable::held_water_hidden` (`boxel_mesh`), que lee dos bytes por modelo
calculados una vez (`sturdy` y `top_floor`).

## Decisión 2: cubos con agua

- Un cubo cutout con `waterlogged` guarda agua (hojas, como en Minecraft desde 1.19). Uno opaco
  (la losa doble), translúcido o invisible nunca, diga lo que diga su propiedad. Un cubo
  translúcido con agua pediría ordenar dos capas translúcidas en una celda, y Minecraft no tiene
  ninguno.
- El cubo se dibuja igual que seco: las mismas caras, capas, AO y luz, pero una cara por bloque
  (fuera del greedy). Tapa sus caras como el seco: contra un cubo opaco y contra el mismo bloque,
  seco o con agua (`MaterialTable::dry`).
- Su agua es la de cualquier bloque con agua (decisión 1): como es cutout, no tapa ninguna cara.
- Para el fluido es una fuente sellada si colisiona entero: no corre, no cuenta para sus vecinos
  y no toma una fuente nueva (`canPassThroughWall`). Romperlo deja su agua. Sin cambios en el
  código de fluidos, luz, físicas, guardado ni lejanía (en la lejanía es sólido, como seco).

## Consecuencias

- Las raíces muestran toda su agua: costados y piso 1/512 hacia adentro y el techo a sus
  alturas. Una hoja cutout en el borde de arriba ya no tapa la superficie.
- Las losas, escaleras, vallas y muros de la demo dan las mismas mallas: los goldens no cambian.
- Un modelo de cajas opaco que Minecraft marca `noOcclusion` (trampillas, andamios, cofres) y
  guarda agua la sigue tapando. Si el juego quiere verla, lo declara cutout.
- Un bloque translúcido con agua (vidrio con agua, si el juego lo declara) no muestra el agua
  que sus cajas cubren: se ve el bloque solo. Para verla, el juego lo declara cutout.
- Cuesta una lectura de tabla y 4 comparaciones por bloque con agua.
- Las caras metidas 1/512 del agua tienen solo ese margen hasta el borde de su celda, no el 1/16
  que el ADR 0025 (sec. 6) daba para todo fluido. El corrimiento de profundidad (2^-16 de la
  distancia) lo supera desde unos 128 bloques, y una cara en ese borde, de verdad detrás del agua,
  puede ganarle en algunos píxeles. Es cosmético e igual en los tres tiers; el ADR 0025 y el
  comentario de `common.wgsl` ya lo dicen.
- Un cubo con agua cuesta sus caras una a una más las de su agua: un techo de 16x16 hojas con
  agua sobre un estanque son 1277 quads, contra 131 secas. Solo en contenido que lo pida.
- Entre una hoja seca y una con agua queda una cara (la de la seca, que ve agua). Entre dos
  secas no hay ninguna. Las hojas de Minecraft dibujan las dos.
- Descartado: tapar el techo solo a 16/16 (suma un quad oculto bajo cada losa de arriba con agua
  y mueve el golden), mantener la regla vieja para los cutout (las raíces siguen secas) y que un
  translúcido no tape nada (el orden translúcido no lo resuelve).
- Descartado: dibujar el cubo con agua en el greedy (su celda debe ser agua para el corte y las
  alturas, ADR 0025) y esconder la cara de la hoja seca contra la mojada (cambia el corte del
  greedy, el camino caliente).
