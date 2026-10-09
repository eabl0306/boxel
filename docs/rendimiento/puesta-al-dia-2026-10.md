# Puesta al día con tope: la rama contra main (2026-10-08)

Issue #3, ADR 0023. Rama `perf/catchup` (849a784) contra su base, main 61c493a. Completa
[entre-frames](entre-frames-2026-10.md), que comparaba el trabajo entre frames encendido y apagado
con un solo binario.

## Resumen

- Con tope 144 la puesta al día después de un borde baja de 22 a 44 %: superficie 30.8 -> 17.4 ms,
  bajo tierra 37.7 -> 22.3, picado 36.2 -> 28.3, bosque 24.8 -> 14.6.
- A 60 fps gana más: superficie 45.8 -> 16.7 ms (un frame), bajo tierra 55.9 -> 33.4.
- Sin tope, con vsync, en la 780M y en las ediciones no cambia nada.
- El ritmo queda igual en la superficie y mejora bajo tierra y en picado. En el bosque salía un
  poco peor, también apagado. La causa era abrir la ventana de pasos sin nada que hacer; ahora el
  mundo la pide solo con trabajo (ver [Ventana solo con trabajo](#ventana-solo-con-trabajo)).
- Metas de la issue: superficie y bajo tierra sí. Picado ~25 no (28.3), pero sin tope hoy tarda 31.

## Cómo se midió

| Binario | Código | md5 |
|---|---|---|
| `stress` base | 61c493a | 3c5da033 |
| `stress` rama | 849a784 (código de 5826331) | 459a22d8 |
| `stress --features trace` base / rama | 61c493a / 849a784 | 25d8277c / 2ed8058e |
| `headless_world` base / rama | 61c493a / 849a784 | b9cc5557 / ec3f55b2 |
| `voxel_world` rama | 849a784 | ec62a2df |

- RTX 5050 (`--adapter nvidia --backend vulkan`, Mailbox), radio 16, 20 s medidos tras 2 s.
  Superficie: overworld con cuevas mc, vuelo a 64. Bajo tierra: igual con la cámara en
  `16,-1000,16,-90,0`. Picado: `dive` a 64 desde ahí. Bosque: vuelo a 32. Con mar (ADR 0021),
  salvo una fila sin mar.
- Tres variantes: main, la rama con `--idle-work off` (apagado) y la rama tal cual (encendido).
  Corridas intercaladas con el orden rotado: 5 por variante con tope 144 (bosque 9) y con vsync, 3
  en el resto. Las rondas 1 a 3 son de antes de un reinicio de la máquina y la 4 y la 5 de
  después.
- Antes de cada corrida: nada compilando y `tools/wait-idle.sh`. Durante la corrida, un vigía cada
  0.5 s. Se tiraron y repitieron 35 corridas que se cruzaron con compilaciones, tests u otro
  `stress` de los otros agentes.
- Refrescos salteados: en el CSV, cada intervalo entre frames presentados dividido por la mediana
  de esos intervalos, redondeado, menos uno.

## Puesta al día con tope 144

Media de las corridas y rango, en el reloj de los frames.

| Caso | main | Apagado | Encendido | Sin tope, main | Sin tope, rama | Meta |
|---|---:|---:|---:|---:|---:|---:|
| Superficie | 30.8 (28.0 a 38.5) | 29.2 | 17.4 (16.2 a 18.4) | 14.0 | 14.1 | ~17 |
| Bajo tierra | 37.7 (34.2 a 39.9) | 37.7 | 22.3 (21.5 a 23.7) | 24.9 | 24.7 | ~27 |
| Picado | 36.2 (32.5 a 38.2) | 39.1 | 28.3 (27.4 a 29.1) | 31.4 | 31.0 | ~25 |
| Bosque | 24.8 (22.4 a 28.1) | 24.4 | 14.6 (13.6 a 16.4) | 8.8 | 8.9 | |
| Superficie sin mar (3) | 28.2 (26.5 a 31.4) | | 17.6 (17.1 a 18.5) | 13.6 | 14.0 | |

Retraso del trabajo (del inicio del frame del cruce al fin del último job, sin redondear a frames)
y frames por cruce. main no lo mide.

| Caso | Apagado | Encendido | Sin tope, rama | Frames, apagado -> encendido |
|---|---:|---:|---:|---:|
| Superficie | 25.8 ms | 14.3 ms | 13.9 ms | 3.9 -> 2.3 |
| Bajo tierra | 35.2 ms | 19.8 ms | 24.4 ms | 5.2 -> 3.1 |
| Picado | 32.7 ms | 23.9 ms | 30.7 ms | 4.2 -> 3.8 |
| Bosque | 21.5 ms | 10.6 ms | 8.8 ms | 3.3 -> 1.9 |

- Todos los cruces terminan antes del siguiente, en las tres variantes.
- Con tope, la rama queda por debajo de sin tope más un período (6.94 ms) en los cuatro casos, y
  por debajo de sin tope bajo tierra y en picado. En la superficie y el bosque la diferencia con
  sin tope es el redondeo a frames: el retraso del trabajo es igual en la superficie y 1.8 ms
  mayor en el bosque.
- Sin mar da lo mismo que con mar. Las metas de la issue (17.1 y 27.3 sin tope) son de antes de la
  luz; hoy, sin tope, la superficie tarda menos (14) y el picado más (31).

## Ritmo con tope 144

main / apagado / encendido, medias.

| Caso | Frames de más de 7.5 ms | Refrescos salteados | Mostrados por segundo | p99 |
|---|---|---|---|---|
| Superficie | 122 / 126 / 117 | 70 / 85 / 80 | 136.6 / 139.4 / 139.6 | 15.5 / 12.9 / 14.0 ms |
| Bajo tierra | 81 / 70 / 73 | 48 / 27 / 28 | 141.2 / 142.3 / 142.2 | 11.5 / 10.2 / 10.2 ms |
| Picado | 119 / 140 / 69 | 76 / 199 / 28 | 139.8 / 133.7 / 142.2 | 12.8 / 16.5 / 10.1 ms |
| Bosque (9 rondas) | 83 / 110 / 112 | 45 / 69 / 68 | 141.4 / 140.1 / 140.2 | 10.6 / 12.0 / 12.0 ms |

- Los refrescos salteados cambian mucho de una corrida a otra con el mismo binario, de 2 a 131,
  según el estado de la GPU (issue #5). Una corrida de main en la superficie (120 mostrados por
  segundo) y una apagada en picado (30 fps, 759 salteados) cayeron en un modo peor. Ninguna
  encendida.
- En las corridas encendidas: unas 1550 a 1640 esperas de la compuerta (como main), 0 a 2 updates sin
  presentar (main, hasta 80) y ningún frame de más de 18 ms.
- Bosque: frames de más de 7.5 ms 83 -> 112 (prueba de permutación, p = 0.07) y salteados 45 -> 68
  (p = 0.17). Apagado da lo mismo que encendido, así que no es el trabajo entre frames. Lo que sí
  corre también apagado es la iteración extra del runner (primero `Poll` y después un paso que
  contesta `Done`, ADR 0023). El A/B sin ventana lo confirmó: ver abajo.

## Pasos entre frames (encendido)

| Caso | Tiempo por frame, media / p99 | Pasos por frame | Pasos fuera de la ventana por corrida | Peor |
|---|---:|---:|---:|---:|
| Superficie | 0.04 / 1.7 ms | 1.3 | 0 a 1 | 0.03 ms |
| Bajo tierra | 0.05 / 2.2 ms | 1.4 | 0 a 2 | 0.04 ms |
| Picado | 0.06 / 2.6 ms | 1.4 | 0 a 5 | 0.49 ms |
| Bosque | 0.02 / 0.14 ms | 1.1 | 0 | 0 |

- Los pasos fuera de la ventana no son 0, pero todos caben en el margen de 0.7 ms. Ningún hueco
  con uno de esos pasos duró más de 7.39 ms.

## Subidas agrupadas

Los resultados que se recogen entre frames se publican juntos en la update siguiente.

| Caso | Mayor subida en un frame, main -> encendido | Mallas por frame p99 |
|---|---:|---:|
| Superficie | 168k -> 153k quads | 156 -> 149 |
| Bajo tierra | 161k -> 198k | 182 -> 223 |
| Picado | 367k -> 341k | 301 -> 320 |
| Bosque | 43k -> 41k | 8 -> 29 |
| Bajo tierra, 60 fps | 259k -> 346k | 330 -> 477 |

Con `--spans` (frames de puesta al día, p99; main / apagado / encendido). Se usan las corridas sin
el modo de 30 fps: 5 de las 12 primeras cayeron en él, así que se repitió el juego completo.

| Caso | `schedule Main` | `render::prepare` | `render.upload` | `schedule Idle` en el frame del cruce, media |
|---|---|---|---|---:|
| Superficie | 4.1 a 4.9 / 3.9 a 4.6 / 4.0 a 4.5 ms | 0.3 / 0.3 a 0.4 / 0.3 a 0.4 | 1.1 / 1.1 / 1.1 | 2.0 a 2.1 ms |
| Bajo tierra | 4.2 a 4.5 / 4.6 / 5.1 ms | 0.5 / 0.5 / 0.9 | 0.9 / 1.0 / 1.1 | 2.7 ms |
| Picado | 6.2 / 6.0 / 5.1 ms | 0.8 / 0.8 / 1.2 | | 2.7 ms |
| Bosque | 4.5 / 4.5 / 2.3 a 3.2 ms | 0.3 / 0.2 a 0.3 / 0.2 a 0.4 | 0.5 / 0.4 / 0.4 | 1.8 a 1.9 ms |

- Bajo tierra se publica un 23 % más por frame a 144 (45 % más mallas a 60 fps), y en las cuevas
  `render::prepare` cuesta unos 0.4 ms más en esos frames. Los frames de más de 7.5 ms y el p99 no
  empeoran.
- `schedule Idle` corre en el tiempo que antes se dormía (dentro de "outside Main"). Por cruce, el
  trabajo de `Main` baja de 13 a 8 ms en la superficie y de 16 a 11 bajo tierra.

## Otros casos

| Caso | main | Rama |
|---|---|---|
| Sin tope, superficie: fps, p50, p99, p99.9 | 1982, 0.46, 1.18, 2.92 ms | 1978, 0.46, 1.18, 2.91 ms |
| Sin tope, bajo tierra | 1503, 0.59, 1.89, 3.18 ms | 1496, 0.60, 1.88, 3.24 ms |
| Sin tope, picado | 1486, 0.60, 1.85, 3.20 ms | 1485, 0.60, 1.84, 3.13 ms |
| Sin tope, bosque | 3696, 0.24, 0.75, 2.52 ms | 3690, 0.24, 0.75, 2.49 ms |
| Vsync, superficie (5): mostrados, salteados, de más de 7.5 ms | 114.1, 590, 626 | 114.2, 587, 620 |
| Vsync, puesta al día | 33.0 ms | 18.6 ms |
| 60 fps, superficie: puesta al día (apagado) | 45.8 ms | 16.7 ms (52.8) |
| 60 fps, bajo tierra | 55.9 ms | 33.4 ms (78.1) |
| 60 fps: mostrados, salteados | 60.0, 0 a 2 | 60.0, 0 a 1 |
| Ediciones: frames hasta publicar, lotes partidos | 2 y 2, 0 | 2 y 2, 0 |
| 780M, superficie (3): mostrados, salteados, de más de 7.5 ms | 138.5, 102, 140 | 139.1, 90, 132 |
| 780M, puesta al día | 35.8 ms | 24.4 ms |
| `headless_world`, salto de 64 chunks a 144 Hz, terreno | 0.28 a 0.35 s (40 a 51 frames) | 0.10 a 0.11 s (15 a 16) |
| Igual, overworld en y = -1000 | 0.38 a 0.41 s (55 a 59) | 0.38 a 0.39 s (55 a 56) |
| `voxel_world`, 15 s, límite 144 y 143: mostrados por segundo | | 142.9 y 142.9 |

- A 60 fps, apagado es peor que main porque la rama quita el crecimiento de presupuestos de
  7281b1d, que a 60 fps sí ayudaba. Encendido gana de lejos.
- Vsync: la mayoría de las corridas muestra 106 a 108 por segundo; dos de main (115 y 137) y una de
  la rama (143) quedaron más arriba. El resto, igual.

## Contra el criterio de la issue

| Criterio | Resultado |
|---|---|
| Puesta al día con tope igual o por debajo de sin tope (~17 / ~27 / ~25) | Superficie 17.4 y bajo tierra 22.3, sí. Picado 28.3, no contra 25, pero sí contra el sin tope de hoy (31.0) |
| Contra sin tope del mismo binario más un período | Sí en los cuatro casos |
| Retraso del trabajo con tope igual a sin tope | Igual en la superficie, menor bajo tierra y en picado, 1.8 ms mayor en el bosque |
| Ningún frame cuyo trabajo pase el período | Sí. Pasos fuera de la ventana hasta 0.49 ms, dentro del margen; ningún frame de más de 18 ms |
| Ritmo no peor que main | Sí en superficie, bajo tierra, picado, vsync y 780M. Bosque: era la ventana sin trabajo, arreglado (ver abajo) |
| Sin cambios sin tope | Sí |

## Ventana solo con trabajo

Diagnóstico del bosque (radio 16, vuelo a 32, tope 144, RTX 5050), 20 s por corrida, en
`bench-results/issue3/diag-forest/` (`variants.txt`, `pairs.txt`, `modes.txt`). "Sin ventana" es
48a0427 con un parche de prueba que nunca abre la ventana, con el trabajo apagado.

| Par | Rondas | Salteados | p | Modo alto de salteados | McNemar p |
|---|---:|---:|---:|---|---:|
| apagado - sin ventana | 46 | +19.3 | 0.018 | 30 contra 18 | 0.023 |
| encendido - sin ventana | 30 | +4.8 | 0.65 | 16 contra 13 | 0.63 |
| encendido - main | 12 | -16.2 | 0.44 | 5 contra 7 | 0.73 |

- Abrir la ventana sin trabajo sube la proporción de corridas en el modo alto (issue #5). El paso
  en sí cuesta 6.5 us de media.
- Arreglo (`IdleWanted`, ADR 0023): el mundo pide pasos solo si uno encontraría trabajo. Apagado
  ahora es igual a main por construcción: ningún paso y la espera de siempre.

Prueba rápida del arreglo (nivel 1): una corrida de 10 s por lado, base 48a0427 (el código de
849a784, `stress` md5 459a22d8) contra la rama b0fbf23 (md5 db15bfd6). Falla con fps por debajo
del 85 % de la base o un p99 de más de 1.5 veces el de la base.

| Caso | fps | p99 | Salteados | Frames con pasos | Puesta al día |
|---|---|---|---|---|---|
| Bosque, apagado | 142.9 -> 143.1 | 7.54 -> 7.56 ms | 8 -> 6 | 100 -> 0 % | 23.9 -> 22.5 ms |
| Bosque, encendido | 143.3 -> 141.3 | 7.57 -> 13.90 ms | 5 -> 24 | 100 -> 1 % | 14.0 -> 14.1 ms |
| Bosque, encendido, repetida | 140.2 -> 143.3 | 14.05 -> 7.51 ms | 35 -> 4 | 100 -> 1 % | 16.2 -> 14.4 ms |
| Superficie, encendido (solo rama) | 141.1 | 13.92 ms | 26 | 3 % | 18.2 ms |

- El primer par encendido no cumplió el criterio del p99: la rama cayó en el modo alto. Repetido,
  el que cayó fue la base. Es el ruido de siempre (issue #5), no una regresión.
- La puesta al día no cambia: bosque 14.1 y 14.4 ms (antes 14.6), superficie 18.2 ms (antes 17.4,
  de 16.2 a 18.4).
- `headless_world`, salto de 64 chunks a 144 Hz: los mismos chunks, mallas y quads (17727, 1801,
  439387), 14 y 15 frames.
- Datos y scripts en `bench-results/issue3/smoke-want/`.

## Para repetir

Los scripts de esta medición quedaron en `bench-results/issue3/harness/` (ignorado por git). Una
corrida:

```bash
GPU="--adapter nvidia --backend vulkan --radius 16"
UNDER="--world overworld --caves mc --scenario fly --speed 64 --camera 16,-1000,16,-90,0"
stress $GPU $UNDER --fps-limit 144 --summary on.md --csv on.csv
stress $GPU $UNDER --fps-limit 144 --idle-work off --summary off.md --csv off.csv
stress $GPU $UNDER --fps-limit 0 --summary uncapped.md
python3 tools/bench_report.py --compare off.md on.md
```
