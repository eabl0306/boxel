# ADR 0041: ticks aleatorios

- Estado: aceptado (el dueño pidió que los cultivos crezcan y que el agua riegue la tierra de
  cultivo, como en Minecraft)
- Fecha: 2026-10-29

## Contexto

El motor tenía ticks programados (ADR 0021): un bloque pide que lo actualicen dentro de tantos
ticks. Minecraft tiene además ticks aleatorios (`randomTickSpeed`): en cada tick elige celdas al
azar de cada sección cercana al jugador, y el bloque que cae ahí decide qué hacer. Así crecen los
cultivos, se seca la tierra de cultivo, crece la caña. Sin eso el juego no puede tener granjas.

## Decisión

- Un bloque registra un `RandomTickHandler` (`BlockTicks::register_random`): una función con el
  mismo `TickContext` que los ticks programados y el alcance que lee (`[horizontal, abajo,
  arriba]`). El motor no nombra ningún bloque.
- En cada tick del reloj de 20 por segundo, cada chunk cargado a menos de
  `random_tick_radius` chunks de un `ChunkViewer` (4 y 4: 128 bloques, la distancia de simulación
  de Minecraft) recibe `random_ticks_per_chunk` elecciones (24: las 3 de Minecraft por sección de
  16 al cubo, por 8 secciones). Con 0 se apagan.
- Un chunk cuya paleta no tiene ningún estado con handler no recibe elecciones: casi todos los
  chunks se descartan con una pasada por su paleta. Sin handlers registrados el sistema no hace
  nada.
- Una elección cuyo alcance no está cargado se salta (no espera, a diferencia de un tick
  programado).
- Las elecciones salen de un hash del tick, del chunk y del número de elección, y
  `TickContext::random` de la posición y el tick: la misma partida da lo mismo. Corren después de
  los ticks programados del frame, leen lo que escribieron y sus escrituras van al mismo lote de
  ediciones.
- Tras una pausa larga se recupera como mucho `random_catch_up` ticks (1 segundo); el resto se
  pierde, como un servidor atrasado.

## Costo

Una pasada por la paleta de los chunks cercanos en cada tick (unos 729 chunks con el radio de 4)
y 24 lecturas por chunk que tiene algo que crece. Con un campo de trigo es poco frente al
presupuesto de los ticks programados. No se midió con perf-smoke: los casos de medición no tienen
bloques con handler, así que el sistema sale en la primera línea.

## Fuera de esta parte

- La luz: el `TickContext` no la lee, así que los cultivos crecen también a oscuras (Minecraft
  pide luz 9). Resuelto en el ADR 0059.
- La lluvia, que en Minecraft también moja la tierra de cultivo.
