# ADR 0012: bucket cutout y pipelines opaco y cutout separados

- Estado: aceptado
- Fecha: 2026-10-05

## Contexto

En el borrador, los bloques cutout (hojas) se dibujaban con el mismo pipeline que el terreno
opaco, con `discard` en el fragment shader y `cull_mode: Back`. Dos problemas: (1) un shader con
`discard` y escritura de profundidad pierde o limita el early-Z en la mayoría de las GPU, así que
**todo** el terreno pagaba ese costo, en contra del objetivo de no perder optimización; (2) bgm
omite las caras entre dos hojas vecinas y las caras traseras se descartaban, así que por los
huecos de una hoja se veía el cielo a través de toda la copa. Cambiar el layout de buckets más
adelante toca mesher, shaders, tabla de chunks y renderer, así que había que decidir antes de
congelarlo.

## Decisión

- 8 buckets por chunk, contiguos en el arena: 0 a 5 caras opacas (orden `Face`), **6 cutout**
  (todas las caras de materiales cutout), 7 translúcido.
- `ChunkGpu` pasa a 64 bytes: origen, `quad_base`, 8 contadores, `flags` (bit 0 = vivo) y relleno.
- Dos pipelines por tier: **opaco** (`fs_opaque`, sin `discard`, `cull_mode: Back`) y **cutout**
  (`fs_cutout`, alpha test, `cull_mode: None`), dibujado después del opaco.
- High: el shader de culling escribe dos listas indirectas (opaca y cutout) con dos contadores, y
  se emite un `multi_draw_indexed_indirect_count` por lista. El bucket cutout solo pasa el test de
  frustum (mezcla caras de todas las direcciones).
- El buffer de índices compartido cubre 98 304 quads (el bucket cutout puede juntar las seis caras
  de un tablero de ajedrez de hojas).
- M0 acepta la omisión de caras entre hojas vecinas (bgm): la copa se ve como una cáscara
  texturada, con las caras interiores visibles por los huecos gracias a `cull_mode: None`. Las
  caras interiores ("hojas elegantes") son una opción de M1 que no cambia el layout.

## Consecuencias

- El terreno opaco conserva el early-Z en todos los tiers.
- Una llamada indirecta más en High y un cambio de pipeline más por frame en todos los tiers.
- `ChunkGpu` crece 16 bytes por slot (512 KiB con 32 768 slots).
- La imagen entre tiers se sigue comparando con escenas que incluyen quads cutout.

## Alternativas descartadas

- Seguir con `discard` en el pipeline único: penaliza todo el terreno.
- Dibujar las hojas como opacas en M0 y decidir después: obliga a cambiar el layout congelado más
  tarde.
- Un flag por quad y dos draws por chunk sobre el mismo bucket: complica el culling y no ahorra
  memoria frente a un bucket propio.
