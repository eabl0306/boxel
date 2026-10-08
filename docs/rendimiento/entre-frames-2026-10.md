# El mundo entre frames: mediciones (2026-10-08)

Issue #3, ADR 0022. Rama `perf/catchup`. Primero el A/B con la máquina libre, que es el que vale.
Al final, la primera medición rápida, hecha con la máquina cargada. La comparación completa contra
main (ritmo, vsync, 60 fps, 780M, spans) está en [puesta-al-dia](puesta-al-dia-2026-10.md).

## A/B con la máquina libre

### Cómo se midió

- Un solo binario de `stress` (md5 d6d786c5, el código de 5826331) en tres modos: tope 144 con
  `--idle-work off`, tope 144 con `--idle-work on` y sin tope (`--fps-limit 0`). Intercalados y en
  orden rotado. `headless_world` es de 74019cd (md5 c257a556).
- RTX 5050 (`--adapter nvidia --backend vulkan --vsync off`, Mailbox), radio 16, overworld con
  cuevas mc, 15 s medidos. Superficie: vuelo a 64. Bajo tierra: vuelo a 64 con la cámara en
  `16,-1000,16,-90,0`. Picado: `dive` a 64 desde ahí.
- Antes de cada corrida: nada compilando y `tools/wait-idle.sh`. Durante la corrida, un vigía
  miraba cada 0.5 s si había un `rustc` o un binario de test de otro agente; si lo había, la
  corrida se tiró y se repitió. Quedaron 3 a 6 corridas limpias por caso.
- La carga pega más en las corridas con tope: de las 9 primeras con tope, 4 se cruzaron con una
  compilación, y ninguna de las sin tope. Una corrida con tope deja la CPU casi libre y ahí
  arranca el trabajo de los otros agentes. Las medias con carga de la primera medición venían de
  eso.

### Puesta al día

Puesta al día después de cada cruce de borde (`border catch-up`, en el reloj de los frames) y
retraso del trabajo (del inicio del frame del cruce al fin del último job, sin redondear a
frames). Media de las corridas y rango.

| Caso | Tope, apagado | Tope, encendido | Sin tope |
|---|---:|---:|---:|
| Superficie (3 / 4 / 4 corridas) | 29.3 ms (25.5 a 32.1) | 16.5 ms (15.1 a 19.4) | 18.3 ms (14.2 a 29.8) |
| Bajo tierra (3 / 4 / 5) | 39.0 ms (38.2 a 40.3) | 23.2 ms (22.7 a 23.9) | 25.5 ms (23.9 a 28.2) |
| Picado (3 / 4 / 6) | 35.0 ms (33.6 a 36.0) | 31.2 ms (29.3 a 34.4) | 36.3 ms (31.6 a 46.7) |
| Retraso del trabajo, superficie | 25.5 ms | 13.8 ms | 18.1 ms |
| Retraso del trabajo, bajo tierra | 36.8 ms | 20.1 ms | 25.2 ms |
| Retraso del trabajo, picado | 31.1 ms | 26.5 ms | 36.0 ms |

- Con el trabajo entre frames, la puesta al día con tope queda igual o por debajo de sin tope en
  los tres casos. En la superficie, la mediana sin tope es 14.7 ms (una corrida de 29.8 sube la
  media) y la de con tope 15.7: la diferencia es el redondeo a la grilla de 6.94 ms, porque el
  retraso del trabajo con tope es menor.
- Metas de la issue: superficie ~17 y bajo tierra ~27 se cumplen. Picado ~25 no, pero sin tope
  este binario tarda 36 ms: el mundo pesa más que cuando se midió esa meta.

### Ritmo

| Caso | Frames de más de 7.5 ms por corrida, apagado / encendido | Mostrados por segundo, apagado / encendido | Sin tope, mostrados |
|---|---|---|---|
| Superficie | 95, 90, 130 / 174, 107, 54, 85 | 138.9 / 139.6 | 134.7 a 143.5 |
| Bajo tierra | 142, 66, 57 / 75, 66, 70, 70 | 141.5 / 141.2 | 142.9 a 143.5 |
| Picado | 62, 93, 58 / 181, 101, 77, 142 | 140.6 / 139.7 | 140.6 a 143.5 |

- Los frames mostrados no cambian: las diferencias quedan dentro del ruido de cada lado.
- Los frames de más de 7.5 ms ya estaban con el trabajo apagado. En la superficie y bajo tierra
  quedan igual o bajan. En el picado suben de 71 a 125 de media, con rangos que casi se tocan:
  no alcanza para decir que empeora, pero hay que repetirlo.
- Pasos entre frames: 1.3 a 1.4 por frame, 0.04 a 0.06 ms por frame de media y 1.7 a 2.5 ms en
  el p99. Pasos fuera de la ventana: de 0 a 7 por corrida, el peor de 0.34 ms, dentro del margen
  de 0.7 ms.

### Tope automático de jobs más chico

Con tope, el tope automático de jobs en vuelo se calcula para frames de 6.94 ms, y sin tope para
frames de menos de 1 ms. Por eso la cola en los workers es mucho más honda con tope (370 a 880
jobs en vuelo, contra 110 a 290 sin tope). Se probó calcularlo para frames de 2 ms con tope (un
cambio solo para medir, no quedó en la rama):

| Caso | Tope, encendido | Encendido, tope de jobs para 2 ms |
|---|---:|---:|
| Superficie | 16.5 ms | 16.5 ms (4 corridas) |
| Bajo tierra | 23.2 ms | 23.9 ms (6) |
| Picado | 31.2 ms | 31.4 ms (6) |

La cola baja a unos 250 jobs y la puesta al día no cambia. No es lo que frena.

### Sin GPU: salto de 64 chunks a y = -1000

`headless_world --world overworld --viewer-y -1000 --view-distance 16 --jump 64`, 3 corridas por
caso. Tiempo desde el frame del salto hasta que la zona nueva está lista.

| Tope 144, apagado | Tope 144, encendido | Sin límite |
|---:|---:|---:|
| 0.396 a 0.410 s (57 a 59 frames) | 0.382 s (55 frames, las 3) | 0.433 a 0.472 s |

Ahí el límite es cuánto trabajo hay: con tope ya era más rápido que sin límite, porque sin
límite el hilo principal corre frames sin parar y le quita una CPU a los workers.

### Para repetir

```bash
GPU="--adapter nvidia --backend vulkan --vsync off --radius 16 --duration 15"
UNDER="--world overworld --caves mc --scenario fly --speed 64 --camera 16,-1000,16,-90,0"
stress $GPU $UNDER --fps-limit 144 --idle-work off --summary off.md
stress $GPU $UNDER --fps-limit 144 --idle-work on --summary on.md
stress $GPU $UNDER --fps-limit 0 --summary uncapped.md
python3 tools/bench_report.py --compare off.md on.md
headless_world --world overworld --viewer-y -1000 --view-distance 16 --jump 64 --hz 144
headless_world --world overworld --viewer-y -1000 --view-distance 16 --jump 64 --hz 0
```

## Primera medición, con la máquina cargada

Medición rápida para ver que el cambio funcionaba. La máquina estaba compartida con otros dos
agentes (carga 2 a 7, CPU ocupada 17 a 40 % antes de varias corridas), y varias corridas tienen
ruido claro. Sus medias con tope son peores que las del A/B de arriba por eso.

### Cómo se midió

- Mismo binario con `--idle-work off` y `--idle-work on`, intercalados. El binario es de 3036b17
  (`stress`, md5 ad3d45a2). `headless_world` es de 74019cd.
- `stress` en la RTX 5050 (`--adapter nvidia --backend vulkan`, Mailbox), radio 16, overworld con
  cuevas mc, `--fps-limit 144`, 15 o 20 s medidos. Superficie: vuelo a 64. Bajo tierra: vuelo a
  64 en y = -1000. Picado: `dive` a 64 desde y = -1000.
- La máquina estaba compartida con otros dos agentes: carga 2 a 7, y `tools/wait-idle.sh` vio
  la CPU ocupada al 17 a 40 % antes de varias corridas. Hay corridas con ruido claro (muchos
  refrescos salteados, cola de jobs alta).

### Resultados con la GPU

Puesta al día después de cada cruce de borde (`border catch-up`): en el reloj de los frames, en
frames, y el retraso del trabajo (del inicio del frame del cruce al fin del último job).

| Caso | Corrida | Apagado: media / peor ms | Encendido: media / peor ms | Frames, apagado -> encendido | Retraso del trabajo, apagado -> encendido |
|---|---|---:|---:|---:|---:|
| Superficie | 1 | 29.1 / 42.8 | 15.3 / 27.7 | 3.6 -> 2.1 | 25.2 -> 13.3 ms |
| Superficie | 2 | 28.6 / 42.1 | 25.0 / 83.9 | 3.7 -> 3.0 | 25.2 -> 21.9 ms |
| Superficie | 3 | 35.0 / 70.1 | 17.1 / 35.2 | 4.2 -> 2.2 | 31.8 -> 14.8 ms |
| Superficie | 4 | 40.4 / 63.4 | 19.5 / 63.2 | 4.4 -> 2.4 | 35.7 -> 17.3 ms |
| Bajo tierra | 1 | 76.4 / 124.7 | 40.8 / 55.9 | 7.2 -> 4.2 | 72.1 -> 36.8 ms |
| Bajo tierra | 2 | 37.1 / 49.7 | 31.1 / 42.0 | 5.2 -> 4.1 | 34.9 -> 28.2 ms |
| Picado | 1 | 75.9 / 104.4 | 29.6 / 35.2 | 6.3 -> 3.5 | 69.8 -> 24.2 ms |
| Picado | 2 | 48.0 / 69.8 | 35.7 / 48.8 | 5.6 -> 4.5 | 44.8 -> 32.9 ms |

Ritmo y pasos ociosos de las mismas corridas:

| Caso | Frames de más de 7.5 ms, apagado / encendido | Mostrados por segundo | Pasos ociosos por frame (encendido) | Tiempo ocioso por frame, media / p99 | Pasos fuera de la ventana (peor) |
|---|---|---|---:|---:|---|
| Superficie | 102, 137, 346, 196 / 97, 205, 109, 246 | 128 a 140 / 136 a 141 | 1.2 a 1.3 | 0.04 / 1.5 a 1.9 ms | 0, 7 (3.1 ms), 0, 3 (4.1 ms) |
| Bajo tierra | 312, 76 / 167, 126 | 129, 142 / 136, 142 | 1.3 a 1.5 | 0.07 / 2.3 a 2.4 ms | 12 (0.24 ms), 3 (3.1 ms) |
| Picado | 229, 176 / 81, 110 | 131, 140 / 139, 141 | 1.4 | 0.05 a 0.07 / 2.4 a 2.5 ms | 0, 8 (0.08 ms) |

- Con el trabajo entre frames la puesta al día baja en todos los casos: en la superficie, de
  33.3 a 19.2 ms de media (4 corridas). El retraso del trabajo baja de 29.5 a 16.8 ms. Sin tope
  y con luz, en e895a11, la superficie tardaba 12.9 ms.
- Las peores corridas de cada lado coinciden con la máquina cargada (muchos refrescos salteados
  o cola de jobs alta). Hace falta la medición completa para el ritmo: frames de más de 7.5 ms y
  mostrados por segundo no muestran una tendencia clara entre el ruido.
- Los pasos que se pasan de la ventana suelen pasarse de 0.02 a 0.25 ms (cubiertos por el
  margen de 0.7 ms). Los de 3.1 a 4.1 ms aparecieron solo en corridas con carga. En una corrida
  instrumentada ningún resultado ni arranque de job tardó más de 0.15 ms: lo más probable es que
  el sistema operativo haya sacado al hilo de la CPU.

### Sin GPU: un salto con `headless_world`

`headless_world --view-distance 16 --jump 64`, terreno, 2 o 3 corridas por caso. `settled_after`
es el tiempo desde el frame del salto hasta que la zona nueva está lista.

| Ritmo | Apagado | Encendido |
|---|---:|---:|
| 144 Hz (`--hz 144`) | 0.250 a 0.312 s (36 a 45 frames) | 0.104 a 0.111 s (15 a 16 frames) |
| 60 Hz | 0.650 a 0.667 s (39 a 40 frames) | 0.117 s (7 frames) |
| Sin límite (`--hz 0`) | 0.097 a 0.140 s | 0.101 a 0.111 s |
| 144 Hz, overworld en y = -1000 (1 corrida) | 0.424 s (61 frames) | 0.396 s (57 frames) |

- Con tope, el salto termina casi tan rápido como sin límite.
- Sin límite no cambia nada: no corre ningún paso ocioso; las diferencias son ruido.
- En las cuevas el salto casi no mejora: ahí el límite es cuánto trabajo hay, no los saltos
  entre etapas.

