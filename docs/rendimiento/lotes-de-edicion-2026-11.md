# Lotes de edición enteros (issue #7, 2026-11-02)

## Qué se midió

Si un lote de ediciones (poner o sacar una lámpara, cavar o tapar un pozo) sale entero en un
frame, también sin tope de fps, después del piso de tiempo del lote (`edit_batch_hold`, commits
cba0e64 y 29e0178).

`just perf-edits`: `stress --edits` en la superficie del overworld con la cámara quieta, radio 16,
RTX 5050, 3 corridas de 30 s sin tope y 3 con tope 144. Una edición por segundo: 180 lotes en
total. Binario de main 1672cbd.

## Resultado

| tope | fps | ediciones | frames p50 / máx | ms p50 / máx | lotes partidos |
|---|---:|---:|---:|---:|---:|
| sin tope | 1966 | 30 | 3 / 5 | 1,82 / 2,58 | 0 |
| sin tope | 1827 | 30 | 3 / 4 | 1,94 / 2,61 | 0 |
| sin tope | 1958 | 30 | 4 / 4 | 1,90 / 3,41 | 0 |
| 144 | 144 | 30 | 2 / 2 | 13,98 / 15,41 | 0 |
| 144 | 144 | 30 | 2 / 2 | 13,97 / 14,36 | 0 |
| 144 | 144 | 30 | 2 / 2 | 13,91 / 14,22 | 0 |

Ningún lote partido, con y sin tope. Antes del piso de tiempo, sin tope se partían de 1 a 3 de
cada 20.

## Latencia de una edición

Desde el frame de la edición hasta que el mundo vuelve a estar quieto, ese frame incluido:

- **Sin tope:** 3 o 4 frames, unos 2 ms (máximo 5 frames, 3,4 ms). Las mallas del lote llegan
  juntas, así que el piso de 16 ms no llega a esperar.
- **Con tope 144:** 2 frames, unos 14 ms (máximo 15,4 ms).

Para repetirlo: `just perf-edits` (detalle en `docs/benchmarks.md`).
