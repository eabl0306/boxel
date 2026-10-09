# ADR 0025: fluidos, segunda parte (costas, lava, bloques con agua y textura de flujo)

- Estado: aceptado (sigue a la issue #38)
- Fecha: 2026-10-09
- Revisa: ADR 0021 (fluidos), ADR 0018 sección 6 (la superficie del agua) y ADR 0020 (modelos de
  cajas)

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
- Reglas (`boxel_world::fluid`), las de `LavaFluid` en el overworld: demora 30 ticks, busca bajada
  a 2 bloques, pierde 2 de cantidad por bloque (niveles 2, 4 y 6: llega 3 bloques) y no hace
  fuentes nuevas. El código es el mismo para los dos fluidos (`FluidRules`); solo cambian esos
  números.
- Fuera: la demora al azar de la lava que sube (`getSpreadDelay`), el fuego y la lava rápida del
  Nether.

### 3. Agua y lava

- Como Minecraft (`LiquidBlock::shouldSpreadLiquid`): una celda de lava con agua arriba o al lado
  se endurece en el acto. Una fuente se vuelve obsidiana y el resto, adoquín. El agua debajo no
  cuenta.
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
- Bloques nuevos al final de la lista: `boxel:cobblestone` y `boxel:obsidian`, cubos opacos con
  textura procedural (un patrón de piedras nuevo, `TexturePattern::Cobble`, y motas violetas) y
  receta de pack (`block/cobblestone`, `block/obsidian`).

### 4. Ids

La lava pasa de 1 a 16 estados: los ids después de ella se corren 15 (vidrio 49, losas desde 50,
plantas 53 y 54, escaleras desde 55, valla desde 215, antorchas desde 231), y adoquín y obsidiana
quedan en 236 y 237. Los ids no se guardan (la persistencia guardará nombres), así que solo cambian
tests.

## Consecuencias

- Los tests de fluidos corren el motor contra el simulador de fuerza bruta, ahora con lava:
  distancia 3 con paso 2, la bajada a 2 bloques, el secado, obsidiana, adoquín y piedra, terreno al
  azar con agua y lava y ediciones, lejos del origen y con presupuesto.
- Una lava sola no se ve como el cubo de antes: su techo baja hacia el aire de alrededor.
- Rendimiento: hoy no se midió (la máquina estaba ocupada). Tocan caminos calientes la generación
  de chunks cerca del mar (la regla de las costas), los ticks de fluidos (la clasificación de
  estados y la reacción de la lava) y el mesher. Quedan para la próxima ronda de control.

## Alternativas descartadas

- Que el agua generada programe ticks donde toca aire: arreglaría la pared haciendo que el agua
  corra al cargar, pero inunda cuevas al explorar y gasta ticks en cada chunk de costa.
- La reacción de la lava en su propio tick (30 ticks después): el agua y la lava convivirían un
  segundo y medio, y el agua podría pasar por encima de la lava en ese tiempo.
- Un mecanismo de reacción solo para la lava dentro de `fluid`: los ticks programados son del
  motor; una reacción genérica sirve también para otros bloques.
