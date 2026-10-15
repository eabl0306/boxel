# ADR 0025: fluidos, segunda parte (costas, lava, bloques con agua y textura de flujo)

- Estado: aceptado (sigue a la issue #38)
- Fecha: 2026-10-09
- Revisa: ADR 0021 (fluidos), ADR 0018 sección 6 (la superficie del agua) y ADR 0020 (modelos de
  cajas)
- Revisado por: ADR 0029 (agua y lava se declaran en el registro, con `boxel_demo::fluids`) y
  ADR 0038 (sección 4: qué caras de su agua tapa un bloque; sección 6: el margen de las caras
  metidas)
- Revisado: 2026-10-14 (#110, ningún fluido que corre entra en un bloque que puede tener agua)

## Contexto

El ADR 0021 dejó el agua corriendo, con lagos y mar, y anotó lo que faltaba: la lava que corre,
las interacciones entre agua y lava, los bloques con agua adentro (`waterlogged`) y la textura de
flujo. Además quedaba un defecto del mundo generado: donde una cueva se abre de costado junto al
mar, el agua queda quieta como una pared vertical, porque el agua generada no programa ticks.

## Decisión

### 1. Las cuevas no se abren junto al mar

- Minecraft lo resuelve en los carvers: no excavan al lado de un fluido. Aquí, el overworld deja
  sólida toda celda que las cuevas excavarían si alguno de sus 4 vecinos horizontales tiene agua
  de mar (`OverworldGenerator::beside_sea`). Abajo del agua ya estaba `Sea::FLOOR`.
- La regla compara alturas de columnas con el nivel del mar: solo enteros, exacta a cualquier
  distancia. Solo aplica entre la columna más baja posible y el nivel del mar.
- Costo: en el camino denso, las alturas vecinas salen de las columnas del chunk; las 4 filas de
  columnas de afuera se calculan solo cuando una celda excavada del borde, dentro de esa franja de
  alturas, las necesita. En el camino rápido bajo la columna más baja del chunk, solo se revisan
  las columnas del borde (sus celdas están al menos `Sea::FLOOR` bajo todas sus columnas).
- Test: bloques de 16x16 columnas de chunks cerca del origen, a ±2^31, a 2^40 y cerca del límite
  del mundo. Ninguna celda de agua tiene aire o una planta al lado o abajo, y cerca del origen la
  regla sí dejó celdas sin excavar (sin ella habría paredes).

### 2. Lava que corre

- `boxel:lava` tiene el mismo `level` 0 a 15 que el agua. Es un fluido (`BlockModel::Fluid`) sin
  colisión, que brilla con la emisión de siempre (`LAVA_EMISSION`, rojo 15) y deja pasar la luz
  como el agua (opacidad 1: el cielo directo se corta).
- Se dibuja opaca, como la capa sólida de Minecraft: sus quads van a los buckets opacos, pero no
  oculta ninguna cara vecina (su techo está más abajo). Por eso `StateFlags::OCCLUDES` y el chequeo
  de opacidad 15 piden un cubo que no sea fluido (`BlockModel::is_solid_cube`), y el mesher trata
  todo fluido como transparente al cortar caras, sea cual sea su forma. La superficie usa las mismas
  alturas por esquina que el agua.
- El techo de una celda cuyas esquinas no están a la misma altura va al bucket cutout
  (`MaterialTable::sloped_bucket`). El renderer salta el bucket +Y de un chunk cuando el ojo está
  por debajo de su min y, y un techo inclinado se ve desde ahí del lado hacia donde baja. El
  bucket cutout no tiene esa prueba y la textura de la lava pasa su alpha test. Ese techo se dibuja
  de los dos lados, como el del agua.
- Reglas (`boxel_world::fluid`), las de `LavaFluid` en el overworld: demora 30 ticks, busca bajada
  a 2 bloques, pierde 2 de cantidad por bloque (niveles 2, 4 y 6: llega 3 bloques) y no hace
  fuentes nuevas. El código es el mismo para los dos fluidos (`FluidRules`); solo cambian esos
  números.
- Fuera: la demora al azar de la lava que sube (`getSpreadDelay`), el fuego y la lava rápida del
  Nether.

### 3. Agua y lava

- Como Minecraft (`LiquidBlock::shouldSpreadLiquid`): una celda de lava con agua arriba o al lado
  se endurece en el acto. Una fuente se vuelve obsidiana y el resto, adoquín. El agua debajo no
  cuenta. Un bloque con agua cuenta como agua.
- Lava que cae sobre agua la vuelve piedra (`LavaFluid::spreadTo`).
- Entre fluidos rige `canBeReplacedWith`: el agua solo cede ante lava que cae; la lava solo ante
  agua, si tiene al menos 4/9 de alto. Un fluido puede pasar por celdas del otro al buscar la
  bajada, como en Minecraft (no frenan el movimiento).
- Mecanismo: `TickHandler::react`, la reacción de un bloque a un cambio en él o en sus 6 vecinos
  (el `neighborChanged` de Minecraft). Para las escrituras de un tick corre dentro de la misma
  escritura, así que la lava nunca convive con el agua de al lado. Para las demás ediciones
  (jugador, herramientas) corre al principio de los ticks del frame siguiente, antes de cualquier
  tick vencido, a la hora del juego de la edición. Si la reacción cambió el bloque, no se programa
  su tick.
- Bloques nuevos: `boxel:cobblestone` y `boxel:obsidian`, cubos opacos con textura procedural (un
  patrón de piedras nuevo, `TexturePattern::Cobble`, y motas violetas) y receta de pack
  (`block/cobblestone`, `block/obsidian`).

### 4. Bloques con agua (`waterlogged`)

**Estados.** Las losas (menos la doble), las escaleras y la valla tienen la propiedad
`waterlogged` de Minecraft (`WATERLOGGED`). En las escaleras ya existía y no hacía nada.

- `StateFlags::WATERLOGGED` marca los estados que guardan una fuente de agua y
  `StateFlags::WATERLOGGABLE` los que podrían. Un cubo completo (la losa doble) no es ninguna de
  las dos cosas, diga lo que diga su propiedad (un cubo cutout sí, ADR 0038).
- El agua no cambia el modelo, la colisión ni la textura del bloque. La valla con agua corta el
  cielo directo como el agua (opacidad 1); losas y escaleras ya lo hacían.

**En el fluido** (`boxel_world::fluid`, las reglas de `FlowingFluid` con contenedores):

- Un bloque con agua es una fuente de agua: tiene tick (`BlockTicks::register_state`, sus estados
  secos no), corre y cuenta como fuente para sus vecinos.
- Entre dos celdas, un fluido pasa solo donde las caras de colisión de ambas, la de cada una hacia
  la otra, dejan abierta parte de la cara (`canPassThroughWall`). El agua de una losa de abajo sale
  por los costados y nunca por el piso.
- Ningún fluido que corre entra en un bloque que puede tener agua, seco o con agua, tenga
  colisión o no (el `LiquidBlockContainer` de Minecraft): el agua no se lleva un liquen ni un
  coral seco, y la lava no los quema ni vuelve piedra uno con agua. Tampoco entra en un bloque
  marcado con `set_keeps_fluids_out` (en Minecraft, la caña de azúcar y los portales). Donde el
  agua haría una fuente nueva, un bloque seco la toma y queda con agua (`canPlaceLiquid`). Issue
  #110.
- El simulador de fuerza bruta de los tests modela todo esto y el motor coincide con él en cada
  tick de juego, también en terreno al azar con losas, vallas y líquenes, secos y con agua, y
  cañas, y lejos del origen.

**En el juego** (`boxel_voxel::rules`, puras, y `voxel_world`):

- `placed_into`: un bloque que puede tener agua, puesto en una fuente de agua, se la queda; en
  agua que corre o en aire queda seco. Es el `getStateForPlacement` de Minecraft.
- `broken`: romper un bloque con agua deja su fuente de agua.
- `voxel_world` además llena un bloque seco cuando se le pone agua encima (la cubeta de Minecraft
  sobre un bloque que puede tener agua), y la losa doble siempre queda seca. La lava pasa a la
  barra de bloques.

**En la malla** (`boxel_mesh`), como la dibuja Minecraft:

- La tabla lee un estado con agua como su agua: el gather pone en la celda la fuente de agua
  (transparente) y guarda el bloque en `PaddedChunk::waterlogged`, ordenado por índice. Así la celda
  corta caras, se fusiona y calcula alturas como agua para sus vecinos: una losa en un lago es
  parte del lago y no hay cara entre ellos.
- Las cajas del bloque se dibujan como las del bloque seco. Una caja vecina seca se oculta contra
  él como contra uno seco.
- Su agua se dibuja sola, como celda inclinada (`SHAPE_FLUID_SLOPE`): en la superficie, con sus
  alturas por esquina; abajo de otra agua o de un bloque, llena (16/16). No se dibuja la cara del
  agua contra una cara sólida del propio bloque (el piso de una losa de abajo, el respaldo de una
  escalera). Las demás caras, menos el techo, van 1/512 de bloque hacia adentro de la celda
  (`GEO_FLUID_INSET_BIT`, `FLUID_INSET`). Minecraft hace lo mismo con 0.001: así la cara del bloque
  en el borde siempre queda delante y no pelea en profundidad con la del agua. El techo del agua,
  que a 16/16 queda en el plano del techo de un escalón o de un poste, pierde ese empate por la
  sección 6. Qué caras tapa el bloque lo precisa el ADR 0038: uno cutout no tapa ninguna.

### 5. Textura de flujo

- Capas nuevas al final: `boxel:water_flow` y `boxel:lava_flow`. Sin pack son un patrón
  procedural (`TexturePattern::Flow`: rayas que bajan por las columnas y se repiten cada 8 filas).
  Con pack salen de `block/water_flow` (con el tinte del agua) y `block/lava_flow`. En Minecraft
  esos sprites miden el doble que un bloque y una cara muestra un cuarto, así que la capa toma el
  cuarto de arriba a la izquierda de un cuadro del doble del tamaño de la capa.
- Son la textura de los costados de los dos fluidos, como en Minecraft. El techo muestra la
  textura quieta, salvo el de una celda inclinada cuyas esquinas tienen pendiente: ahí el mesher
  pone la de flujo. La dirección es `fluid_flow`, entera: la diferencia de altura entre los dos
  lados de la celda en x y en z. Cero es un techo a nivel.
- Forma nueva `SHAPE_FLUID_CUBE` (código 2, libre desde el ADR 0020): las caras de cubo de las
  celdas de fluido que no son superficie, para que los costados de una cascada también se animen.
  Se dibuja como `SHAPE_CUBE`.
- El shader desplaza la textura de flujo con el tiempo: hacia abajo en los costados, y pendiente
  abajo en un techo inclinado, girada para que su v siga la dirección de `fluid_flow`, que el
  shader calcula igual desde las alturas. El agua avanza media textura por segundo y la lava
  (el fluido emisivo) un cuarto.
- El tiempo va en `ViewUniform::viewport.z`, una componente que estaba libre: el tier Low sigue con
  un solo bloque de uniforms. El plugin de render lo toma del tiempo de la app y lo envuelve cada
  60 s (`FLUID_TIME_PERIOD`): las dos velocidades dan texturas enteras en ese período, así no hay
  salto y el valor sigue chico para `f32`. `snapshot --fluid-time S` fija el momento de la
  animación (0 por defecto: las mismas imágenes en cada corrida).
- Es el mismo código en los tres tiers. El test de tiers dibuja la escena especial, ahora con
  bloques con agua y lava, en otro momento de la animación, y los tres tiers coinciden píxel a
  píxel.

### 6. Empates de profundidad

- Un fluido puede tocar caras en su mismo plano: el lado de atrás del vidrio o de las hojas junto a
  la lava, o el techo de un escalón o de un poste cuando el agua de su bloque sube a 16/16. Esas
  caras se dibujan sin descartar las traseras o después del fluido, con `Greater` estricto, y con
  profundidades iguales el redondeo elige un ganador por píxel.
- El shader escala la profundidad de todo quad de fluido por `FLUID_DEPTH_SCALE` (1 - 2^-16):
  queda esa fracción de su distancia más atrás y pierde todos los empates. Es unas 100 veces el
  redondeo de `f32` y, hasta 4096 bloques, menos que el 1/16 de bloque que separa una cara de
  fluido en una dieciseisava de lo que de verdad está detrás. Los vértices no se mueven en
  pantalla, así que no abre grietas.
- Una cara metida 1/512 (el agua de un bloque con agua, sección 4) tiene solo ese margen hasta el
  borde de su celda. Desde unos 128 bloques el corrimiento lo supera, y una cara en ese borde, que
  está de verdad detrás, puede ganarle en algunos píxeles. Es cosmético e igual en los tres tiers
  (ADR 0038).
- `gpu_fluids` lo prueba en los tres tiers, junto con el techo inclinado de la lava visto desde
  abajo de su chunk.

### 7. Ejemplos

`snapshot` muestra cada parte (las imágenes del dueño están en `shots/fluids2/`, fuera de git):

- costa: `--camera 236.5,53,147,0,-30` en el overworld. Con `--place air@...` en las 9 celdas
  que la regla deja sólidas (y = 46, de x 232 a 240) se ve la pared de agua de antes;
- lava, adoquín, obsidiana y piedra: `--fill stone@0,100,0:22,100,20 --place lava@4,101,15
  --place water@12,101,15 --place lava@16,101,4 --place water@17,101,4 --flow 400`;
- textura de flujo: un canal con escalones y fuentes en la cabecera, con `--fluid-time 0` y
  `--fluid-time 0.5` (la textura avanzó media vuelta) y con `--pack`.

### 8. Ids

- La lava pasa de 1 a 16 estados: los ids después de ella se corren 15.
- Las losas pasan de 3 a 6 estados y la valla de 16 a 32 por `waterlogged`.
- Queda así: lava 33 a 48, vidrio 49, losas 50 a 55, plantas 56 y 57, escaleras de piedra desde
  58 y de roble desde 138, valla 218 a 249, antorchas 250 a 254, adoquín 255 y obsidiana 256.
- Texturas: adoquín y obsidiana son las capas 22 y 23, y las de flujo de agua y lava, 24 y 25.
- Los ids no se guardan (la persistencia guarda nombres), así que solo cambian tests.

## Consecuencias

- Los tests de fluidos corren el motor contra el simulador de fuerza bruta, ahora con lava y
  bloques con agua: distancia 3 con paso 2, la bajada a 2 bloques, el secado, obsidiana, adoquín y
  piedra, losas y vallas que sueltan agua por sus caras abiertas, bloques secos que toman una
  fuente nueva, y terreno al azar con agua, lava y bloques con agua, lejos del origen y con
  presupuesto.
- El motor de los tests corre todos los ticks vencidos en su frame. Con la máquina cargada, el
  presupuesto de 1 ms atrasaba una escena grande y la comparación tick a tick fallaba. El
  presupuesto tiene su propio test.
- Una lava sola no se ve como el cubo de antes: su techo baja hacia el aire de alrededor.
- Una losa o una escalera en un lago cuestan un quad de agua propio arriba (no se fusiona con el
  greedy del lago) y parten el quad del lago a su alrededor.
- El techo inclinado de la lava pasa por el alpha test (sin early-Z en sus píxeles) y puede sumar
  un draw por chunk, el de su bucket cutout.
- Rendimiento: al integrar no se midió (la máquina estaba ocupada). Tocan caminos calientes la
  generación de chunks cerca del mar (la regla de las costas), el gather del mesher (los bloques
  con agua en la pasada de niveles) y su paso de fluidos. Quedan para la próxima ronda de control.
- Los ticks de fluidos se midieron después. Ticks por cada ms de presupuesto en una inundación
  (`cargo bench -p boxel_world --bench fluid`, 10 inundaciones por escena, una corrida por lado):

  | Escena | Antes de esta parte (cf11b4f) | Con esta parte (0313bf2) | Con la memoria |
  |---|---|---|---|
  | Agua | 225 | 174 | 398 |
  | Lava y agua | no existía | 193 | 338 |
  | Bloques con agua | no existía | 135 | 273 |

  - Esta parte le costó un 23 % al agua común. Un tick que corre pasa casi todo su tiempo
    buscando la bajada (`getSlopeDistance`): prueba cada camino de hasta 4 bloques y lee las
    mismas celdas una y otra vez. Las reglas nuevas encarecieron cada paso: lee también la celda
    de la que sale y revisa la pared entre las dos.
  - Arreglo: una memoria por búsqueda (`Memo`), como la caché de `getSpread` en Minecraft. Lee
    cada bloque de la zona una sola vez y guarda, por celda, si el fluido pasa y si cae ahí. La
    pared solo se revisa al salir de una celda con colisión, y eso solo puede pasar en la
    primera. Los bloques en que se vuelve la lava se buscan solo cuando hacen falta.
  - Es exacta: los tests del simulador de fuerza bruta pasan sin cambios, y un test de
    propiedades compara la búsqueda con memoria con la de siempre en mundos al azar, cerca y
    lejos del origen.
  - El bench ahora adelanta el reloj al poner las fuentes. Así la cola sigue atrasada hasta el
    final y cuenta todos los ticks de la inundación, los mismos en cualquier versión. Antes
    contaba solo los frames atrasados, que eran menos cuanto más rápida la versión. Tiene además
    escenas de lava y de bloques con agua.

## Fuera de esta parte

- Nadar dentro de un bloque con agua: la física (`SwimPlugin`) mira solo los estados de agua.
  Queda para la pista de la física.
- La animación por cuadros de las texturas quietas (las de los packs muestran su primer cuadro).
- Guardar los ticks con los chunks (llega con la persistencia).

## Alternativas descartadas

- Que el agua generada programe ticks donde toca aire: arreglaría la pared haciendo que el agua
  corra al cargar, pero inunda cuevas al explorar y gasta ticks en cada chunk de costa.
- La reacción de la lava en su propio tick (30 ticks después): el agua y la lava convivirían un
  segundo y medio, y el agua podría pasar por encima de la lava en ese tiempo.
- Un mecanismo de reacción solo para la lava dentro de `fluid`: los ticks programados son del
  motor; una reacción genérica sirve también para otros bloques.
- Guardar el agua de los bloques en una capa aparte del chunk: Minecraft la guarda en el estado y
  con eso alcanza; una capa más pediría su propio guardado, sus ediciones y su malla.
- Dibujar el agua de un bloque con agua en el greedy y recortar sus costados donde el bloque los
  cubre: pide la altura de abajo de cada costado en el quad, y los bits libres no alcanzan. El
  desplazamiento hacia adentro es un bit y es lo que hace Minecraft.
- Guardar la dirección del flujo en el quad: las alturas de las esquinas ya la dan, y el mesher y
  el shader la calculan igual con enteros.
- Para los empates, meter hacia adentro todas las caras de los fluidos, como Minecraft: en una
  esquina cóncava las dos caras metidas dejan una rendija por la que se ve el interior de la lava,
  que no dibuja sus caras traseras.
- Ocultar la cara del vidrio o de las hojas contra la lava: falla donde la lava no llega arriba de
  la celda, porque su techo baja.
- Para el techo inclinado, una marca por chunk que no salte su bucket +Y: cambia el culling de los
  tres tiers.
