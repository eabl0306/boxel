# ADR 0043: hasta 65 536 estados de bloque

- Estado: aceptado (el dueño pidió todos los bloques, ítems y recetas de Minecraft 26.3 salvo
  redstone y marcos; no entraban en 32 768 estados)
- Fecha: 2026-10-30

## Contexto

El registro admitía como mucho 2^15 = 32 768 estados. El motivo era el mesher: el material de un
vóxel es el id del estado, y el gather marcaba las celdas de superficie de los fluidos con el bit
alto de ese id (`FLUID_SURFACE_BIT`, ADR 0018). Con más estados el agua perdía la superficie baja.

Al juego le faltan unos 28 900 estados (muros, maderas, tintes, cobre). Con los que ya tiene y
las rocas del plan de biomas no entran en 32 768.

## Decisión

- `MAX_STATES` pasa a 2^16 = 65 536: todo id `u16` es un estado. Los estados siguen siendo `u16`,
  así que la memoria de los chunks y el formato no cambian.
- La marca de superficie sale del id. Va en una máscara de columnas propia,
  `PaddedChunk::surface`, con el mismo esquema que las otras tres (fila `(x+1) + (y+1)*34`, bit
  `z+1`). El gather la llena; el id de material queda limpio.
- El culling compara ids enteros: ya no hace falta `cull_mask`.
- La fusión greedy sigue separando la capa de superficie del resto del fluido. La marca va en la
  clave: bit 16 sin AO, bit 24 con AO, y `FaceKey::surface` en la tabla de claves con luz.
- Sin AO y con luz uniforme el mesher fusionaba directo sobre los ids. Si el chunk tiene celdas
  de superficie, ahora escribe una clave por cara visible. Sin superficie hace lo de antes. El
  LOD nunca marca superficie, así que no cambia.
- La salida es la misma quad por quad: los tests golden y los conteos de `headless_world` no se
  movieron. Una celda de superficie sigue sin iluminarse plana como emisor (antes su id caía fuera
  de la tabla); se dejó así para no cambiar ninguna imagen.
- Los mundos guardados cargan igual: el formato guarda nombres de estado, no ids (ADR 0024).
- Se arreglaron los lugares que pasaban la cantidad de estados a `u16` (`0..n as u16`,
  `first + count`): con 65 536 daban 0 o desbordaban.

## Medición

`headless_world` (radio 8, `--hz 0`): mismos chunks y quads antes y después en terrain, forest,
overworld, forest sin luz con AO apagado y con AO, y en el LOD lejano (`--lod near`).

| mundo | chunks | quads antes | quads después |
|---|---|---|---|
| terrain | 5111 | 112 316 | 112 316 |
| forest | 5111 | 248 839 | 248 839 |
| overworld | 5111 | 1 290 649 | 1 290 649 |
| forest, luz off, AO off | 5111 | 165 265 | 165 265 |
| forest, luz off, AO baked | 5111 | 236 410 | 236 410 |
| forest, LOD near | 3307 nodos | 3 090 500 | 3 090 500 |

Bench `mesh` de `boxel_mesh`, una corrida por lado. La mayoría queda igual o un poco mejor (el
culling ya no enmascara ids). Lo único que empeora es `mesh/terrain`, el camino sin luz y sin AO
con agua: de 29,6 a 31,4 µs (+9 %), por las claves por cara. Ese camino solo se usa con luz
uniforme y sin AO (por ejemplo una cueva a oscuras con un lago).

| bench | antes | después |
|---|---|---|
| mesh/terrain | 29,6 µs | 31,4 µs |
| mesh/terrain_baked | 46,3 µs | 47,5 µs |
| mesh/surface (con luz) | 105,3 µs | 97,0 µs |
| mesh/cave | 24,5 µs | 22,5 µs |
| gather_mesh/surface | 189,3 µs | 160,8 µs |

`just perf-smoke surface forest` (RTX, radio 16, sin tope), sale con 0:

| caso | fps | p99 ms | frames > 6.94 ms | veredicto |
|---|---:|---:|---:|---|
| rtx/surface/r16/uncapped | 2147 -> 2147 (+0.0 %) | 1.04 -> 1.03 | 0 -> 0 | ok |
| rtx/forest/r16/uncapped | 3923 -> 3937 (+0.3 %) | 0.66 -> 0.66 | 0 -> 0 | ok |

## Consecuencias

- El juego puede subir su presupuesto de estados (de 31 000 a 62 000).
- Pasar de 65 536 pide estados de 32 bits (opción b del plan de bloques): memoria de chunks y
  mesher.
- Una celda de superficie de un fluido que emite luz sin brillo pleno no se ilumina plana,
  mientras que la misma celda en pendiente sí. Venía de antes; arreglarlo cambia imágenes y queda
  para cuando el dueño lo pida.
