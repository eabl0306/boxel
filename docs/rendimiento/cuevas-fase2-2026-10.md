# Cuevas, fase 2: costo de generar el overworld (2026-10-06)

Qué se midió: `OverworldGenerator` v1 (superficie de hoy, piedra infinita hacia abajo, cuevas tipo
Minecraft 1.18 y árboles). Es la fase 2 del plan de cuevas. El aspecto de las cuevas todavía no
está aprobado: los valores por defecto son un punto de partida.

## Resultado

Todo dentro de los objetivos de la fase 2.

| Medida | Valor | Objetivo |
|---|---:|---:|
| `caves/cave_chunk` (chunk profundo con 5.7 % de aire) | 24.1 us | 60 us o menos (ideal 40) |
| `caves/cave_chunk_far` (x = 2^40, y = -2^40) | 20.6 us | hasta 1.3 veces el cercano |
| `caves/solid_chunk` (las cuevas no lo tocan) | 13.5 us | 25 us o menos |
| `caves/surface_chunk_caves` (superficie, cuevas y árboles) | 81.8 us | 90 us o menos |
| `caves/deep_chunks_mean` (64 chunks profundos seguidos) | 23.7 us por chunk | - |
| `caves/density_corners_405` (solo la densidad de las 405 esquinas) | 16.1 us | - |
| `gather_mesh/cave` (el chunk de `cave_chunk`, 677 quads) | 80.5 us | - |
| `terrain/surface_chunk` (referencia, sin cuevas ni árboles) | 49.8 us | - |

Los árboles salían caros: en `ForestGenerator` cuestan unos 60 us por chunk de superficie, porque
`ChunkData::set` vuelve a empaquetar la paleta. En el overworld se ponen en el arreglo denso antes
de codificarlo y cuestan unos 3 us.

## Carga con `headless_world`

Radio 16, radio vertical 8, bucle a 144 Hz, 4 corridas intercaladas de cada caso (los conteos son
idénticos entre corridas):

| Mundo | Altura | Carga | Chunks | Mallas no vacías | Quads | Fracción con malla | Quads por malla p50 / p99 / máx | Vóxeles |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `terrain` | 80 | 0.08-0.09 s | 17 727 | 1 801 | 0.22 M | 0.13 | 53 / 563 / 732 | 13.2 MiB |
| `overworld` | 80 | 0.21 s | 17 727 | 6 706 | 4.10 M | 0.50 | 525 / 1 918 / 2 826 | 47.2 MiB |
| `overworld` | -1000 | 0.34-0.35 s | 17 727 | 12 922 | 8.17 M | 0.95 | 540 / 1 975 / 2 706 | 65.3 MiB |

- Objetivo de carga: 0.6 s o menos en las dos alturas. Se cumple.
- Rangos esperados bajo tierra: fracción con malla 0.6-1.0 y p50 de 400-1 200 quads. Se cumplen.
- A -1000, `--caves closed` da exactamente lo mismo que `mc`: las entradas y el techo solo
  cambian algo cerca de la superficie.
- Bajo tierra a radio 16 hay 8.17 M quads, apenas por debajo del arena por defecto de 8 Mi. Por
  eso el overworld usa 24 Mi quads y 65 536 slots en los ejemplos.

## Aspecto por defecto

Medido lejos de la superficie: un 10 % del volumen es aire (cavernas 6 %, espaguetis 3 %, fideos
1 %, cada uno por separado). Un 0.3 % de las columnas de superficie se abre a una cueva. La perilla
`density` es fuerte: -0.05 deja un 3 % de aire y +0.05 un 23 %.

## Después de la revisión

Medido igual, con las mismas cuevas para comparar:

- Las capas -1 y -2 (bajo la superficie, dentro del alcance de las cuevas) ya no calculan las
  32x32 alturas, solo las 9x9 de las esquinas. `caves/shallow_chunk` (capa -1 bajo la columna
  (0, 0)) bajó de 29.3 a 15.4 us.
- El ruido 3D y las esquinas reusan sus buffers por hilo: de 3 a 6 % menos por chunk profundo
  (`deep_chunks_mean` de 23.5 a 22.7 us).

Las semillas de las cuevas ahora mezclan la semilla del mundo y la sal (antes, sales o semillas
vecinas compartían octavas). Las cuevas cambiaron de lugar pero no de aspecto: medido de nuevo, 10 %
de aire en profundidad y 0.35 % de columnas de superficie abiertas. Con las cuevas nuevas:
`deep_chunks_mean` 22.0 us, `surface_chunk_caves` 85.4 us (objetivo 90) y `solid_chunk` 14.0 us.
`cave_chunk` cae ahora en otro chunk (26 % de aire en vez de 5.7 %): no se compara con la tabla.

`headless_world` a radio 16 con las cuevas nuevas (2 corridas por altura): carga en 0.20 s a 80 y
en 0.34-0.35 s a -1000. Bajo tierra quedan 8.41 M quads en 12 981 mallas (fracción 0.96, p50 572):
ahora pasan los 8 Mi del arena por defecto, así que los 24 Mi del overworld hacen falta.

## Cómo se midió

- Criterion en release, con `tools/wait-idle.sh 60` antes y nada compilando.
- La RTX 5050 no estaba disponible. Nada de esto usa la GPU.
- Commits de la rama `caves/v1`: c908cea (generador) y c83f6e2 (ejemplos).
