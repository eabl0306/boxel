# Línea base de rendimiento (octubre 2026)

Primera medición con `examples/stress.rs` ([cómo usarlo](../benchmarks.md)), para comparar cada
optimización contra esto.

- Fecha: 2026-10-05, commit `e933486`, build release.
- Máquina: Ryzen 7 250 (16 hilos), RTX 5050 Laptop (Vulkan, tier High) y Radeon 780M (GL,
  tier Mid, radeonsi). Wayland, ventana de 1280x720.
- Cada corrida: espera a que el mundo cargue, 2 s sin medir y 20 s medidos. Sin límite de fps.
- Carga del sistema (load average) entre 1.8 y 5.4: había otros procesos abiertos.
- Comando de cada corrida: `stress --scenario fly --speed 32 --radius 16 --backend vulkan
  --adapter nvidia --tier high --duration 20 --warmup 2` (cambiando escenario, radio y
  velocidad; la de GL con `WGPU_BACKEND=gl --tier mid`).

Radio 16 de boxel equivale a los 32 chunks del juego en Bevy (sección "Escala" de
[voxeles.md](../voxeles.md)).

## En resumen

1. **Radio 12 y 16 en Vulkan: ningún frame con trabajo del mundo pasa de 6.94 ms** (144 Hz). El
   peor frame al cruzar un borde de chunk fue de 3.4 ms. Ningún frame pasó de 16.7 ms en ninguna
   corrida.
2. **Radio 32: el único tirón del mundo es cruzar un borde.** Esos frames tardan 6 ms de mediana
   y hasta 9 ms: 5 cruces de 20 (a 32 vóxeles/s) y 5 de 40 (a 64) se pasan de 6.94 ms. Es el
   recálculo entero del interés ([voxeles.md](../voxeles.md), punto 7.1). Es lo primero para
   arreglar.
3. **La carga no se atrasa.** Después de cada borde, el mundo termina los chunks nuevos en 10 a
   48 ms (Vulkan), mucho antes del cruce siguiente (0.5 s a 64 vóxeles/s). La cola nunca crece.
4. **En la NVIDIA hay frames lentos sin trabajo del mundo**: de 1.5 a 5 ms, unas 8 a 18 veces por
   segundo. Son los que marcan el p99.9 (2.3 a 3.6 ms). Dos veces llegaron a 8.9 y 10.1 ms (static
   radio 12 y 16). En una prueba corta con la AMD en Vulkan no aparecieron. Hipótesis: vienen del
   driver o de la presentación, no del motor.
5. **GL Mid en la AMD** hace 1932 fps, unas 4 veces menos que Vulkan High. Tuvo 15 frames de más de
   6.94 ms (máximo 13.6 ms); 13 de esos 15 no tenían cruce de borde ni mallas nuevas.
6. **Ojo:** sin límite de fps el bucle da 3300 a 8500 vueltas por segundo, y por eso la carga
   atada a los frames ([voxeles.md](../voxeles.md), punto 7.2) no se nota. Con el límite a 144 fps
   hay que medir todo de nuevo.

Los fps promedio varían hasta un 45 % entre corridas parecidas (por ejemplo, radio 12: 5900 a
8500) sin un patrón claro: es ruido de la máquina. Para comparar sirven más el p99, el máximo, los
frames de más de 6.94 ms y los frames de cruce.

## Vulkan High, RTX 5050

| Corrida | fps prom. | p50 ms | p99 ms | p99.9 ms | máx. ms | 1% low fps | > 6.94 ms | cruce de borde p50 / máx. ms | atraso tras borde prom. / máx. ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| static r12 | 6965 | 0.12 | 0.46 | 2.68 | 8.85 | 871 | 1 | - | - |
| fly r12 32 vox/s | 6472 | 0.13 | 0.46 | 2.59 | 7.25 | 906 | 1 | 1.32 / 2.63 | 13 / 30 |
| fly r12 64 vox/s | 8480 | 0.09 | 0.41 | 2.46 | 6.62 | 1028 | 0 | 1.24 / 2.07 | 10 / 17 |
| spin r12 | 5893 | 0.14 | 0.46 | 2.64 | 5.14 | 924 | 0 | - | - |
| static r16 | 5892 | 0.14 | 0.47 | 2.54 | 10.11 | 946 | 1 | - | - |
| fly r16 32 vox/s | 7653 | 0.11 | 0.49 | 2.34 | 4.46 | 959 | 0 | 1.64 / 3.35 | 13 / 15 |
| fly r16 64 vox/s | 6348 | 0.13 | 0.46 | 2.66 | 5.45 | 920 | 0 | 2.14 / 2.84 | 17 / 22 |
| spin r16 | 6704 | 0.12 | 0.45 | 2.49 | 5.39 | 960 | 0 | - | - |
| static r32 | 3450 | 0.25 | 0.87 | 2.83 | 5.22 | 650 | 0 | - | - |
| fly r32 32 vox/s | 3308 | 0.25 | 0.96 | 2.93 | 8.43 | 596 | 5 | 6.14 / 8.43 | 44 / 48 |
| fly r32 64 vox/s | 3605 | 0.24 | 0.71 | 3.57 | 8.96 | 625 | 5 | 5.96 / 8.96 | 39 / 44 |
| spin r32 | 3641 | 0.25 | 0.69 | 2.75 | 4.28 | 825 | 0 | - | - |

A radio 32, los 10 frames de más de 6.94 ms son todos cruces de borde: en cada uno se pidieron
unos 390 trabajos nuevos y se borraron unas 150 mallas. En radio 12 y 16, los 3 frames de más de
6.94 ms no tenían trabajo del mundo (punto 4 del resumen).

## GL Mid, Radeon 780M

| Corrida | fps prom. | p50 ms | p99 ms | p99.9 ms | máx. ms | 1% low fps | > 6.94 ms | cruce de borde p50 / máx. ms | atraso tras borde prom. / máx. ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| fly r16 32 vox/s | 1932 | 0.43 | 1.24 | 3.55 | 13.64 | 472 | 15 | 2.91 / 10.82 | 51 / 115 |

## Carga del mundo al volar

| Corrida | chunks generados/s | mallas/s | trabajos en cola, máx. |
|---|---:|---:|---:|
| Vulkan r12 32 vox/s | 162 | 61 | 152 |
| Vulkan r12 64 vox/s | 324 | 119 | 152 |
| Vulkan r16 32 vox/s | 210 | 79 | 200 |
| Vulkan r16 64 vox/s | 420 | 158 | 200 |
| Vulkan r32 32 vox/s | 402 | 157 | 392 |
| Vulkan r32 64 vox/s | 804 | 318 | 392 |
| GL r16 32 vox/s | 210 | 79 | 200 |

En ningún frame se subieron más de 10 mallas (unos 3600 quads): con tantos frames por segundo, las
mallas llegan repartidas y la subida no pesa.

## Qué medir después

1. Interés incremental (paso 2 del plan de [voxeles.md](../voxeles.md)): repetir `fly r32` y
   comparar los frames de cruce. Hoy: 6 ms de mediana.
2. Cuando exista el límite de fps: repetir todo con el límite a 144 y sin límite.
3. Los frames lentos sin trabajo en la NVIDIA: perfilar con tracy para ver de dónde salen.

## Archivos

- Un resumen por corrida en [baseline-2026-10/](baseline-2026-10/): tablas y un bloque JSON
  para `tools/bench_report.py`.
- Los CSV (una fila por frame) no se suben: se regeneran corriendo de nuevo.
- Para comparar una corrida nueva: `just bench-report --compare
  docs/rendimiento/baseline-2026-10/ bench-results/`.
