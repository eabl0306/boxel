# ADR 0005: chunks de 32³ con contenedor de paleta

- Estado: aceptado (enmendado 2026-10-06 por ADR 0015: la luz sale del chunk, caché de máscaras)
- Fecha: 2026-10-04

## Contexto

binary greedy meshing necesita columnas con borde de hasta 64 bits (62 + 2). 62³ da ~20 % más de
rendimiento de meshing por vóxel, pero no es potencia de dos (coordenadas con división, LOD por
mips imposible) y un remesh tras una edición cuesta ~0.56 ms contra ~0.1 ms en 32³. El worldgen,
no el mesher, domina el costo (26 ns/vóxel contra ~1 ns/vóxel).

## Decisión

- Chunk de 32³ (`CHUNK_SHIFT = 5`), índice local `(y << 10) | (x << 5) | z`.
- Contenedor de paleta estilo Minecraft: `Uniform | Packed(1/2/4/8 bits) | Direct(u16)`, índices
  que no cruzan palabras, crecimiento en el lugar, compactación al guardar.
- `BlockState(u16)` denso en tiempo de ejecución, nunca persistido crudo.
- Luz separada; ocupación de 4 KiB por chunk (mantenida desde M1).
- `ChunkData` compartido como `Arc` con copy-on-write. Los chunks no son entidades: `ChunkMap`
  (slotmap + hash) como recurso.
- Cada job de chunk devuelve exactamente un resultado (generado, mallado, cancelado o fallido),
  para que un pánico o una cancelación no dejen ocupado un lugar en vuelo.

## Consecuencias

- Memoria medida: 0.14 B/vóxel en un mundo de terreno (60 % de chunks aire uniforme).
- Más chunks y más draws que con 62³ (aceptable con GPU-driven).
- Lecturas de vóxeles sueltos pasan por el decode de la paleta (rápido; el mesher decodifica en bloque).

## Alternativas descartadas

- 62³ o 64³: peor latencia de edición y LOD.
- Arrays planos de u16 por chunk: 64 KiB por chunk, 14x más memoria.
- Chunks como entidades con componentes de vóxeles: copia de componentes grandes en cambios de
  arquetipo (7 µs a 32 KiB medidos) y mezcla de responsabilidades.

## Enmienda (2026-10-06, ADR 0015)

- **La luz sale de `ChunkData`.** Es un dato derivado: una función pura de los bloques y de un
  techo de cielo por columna, calculada en los jobs (`boxel_light`) y nunca persistida.
  `ChunkData.light` y `LightStorage`, que siempre valían cielo 15, se borraron.
- **La ocupación pasa a ser una caché perezosa**, no algo mantenido en cada edición:
  `ChunkMasks` (opaca = opacidad 15, parcial = opacidad 1 a 14, sólida para física y raycasts,
  y la lista de emisores), construida la primera vez que se pide (`ChunkData::masks`) y compartida
  por todos los lectores del snapshot. `set` y `blocks_mut` la tiran, `clone` (y por lo tanto
  `Arc::make_mut` con el snapshot compartido) empieza sin ella y la igualdad la ignora.
- `ChunkData.blocks` pasa a privado (`blocks()`, `blocks_mut()`, `from_paletted()`, `compact()`)
  para que ningún cambio se saltee la caché.
