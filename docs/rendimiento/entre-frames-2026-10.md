# El mundo entre frames: medición rápida (2026-10-08)

Issue #3, ADR 0022. Rama `perf/catchup`. Es una medición rápida para ver que el cambio funciona;
la comparación completa (contra main, sin tope, vsync, 60 fps, ediciones, la 780M y `--spans`)
queda para otra pasada.

## Cómo se midió

- Mismo binario con `--idle-work off` y `--idle-work on`, intercalados. El binario es de 3036b17
  (`stress`, md5 ad3d45a2). `headless_world` es de 74019cd.
- `stress` en la RTX 5050 (`--adapter nvidia --backend vulkan`, Mailbox), radio 16, overworld con
  cuevas mc, `--fps-limit 144`, 15 o 20 s medidos. Superficie: vuelo a 64. Bajo tierra: vuelo a
  64 en y = -1000. Picado: `dive` a 64 desde y = -1000.
- La máquina estaba compartida con otros dos agentes: carga 2 a 7, y `tools/wait-idle.sh` vio
  la CPU ocupada al 17 a 40 % antes de varias corridas. Hay corridas con ruido claro (muchos
  refrescos salteados, cola de jobs alta).

## Resultados con la GPU

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

## Sin GPU: un salto con `headless_world`

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

## Para repetir

```bash
GPU="--adapter nvidia --backend vulkan"
SURF="--world overworld --caves mc --scenario fly --speed 64"
stress --radius 16 $GPU $SURF --fps-limit 144 --idle-work off --summary off.md
stress --radius 16 $GPU $SURF --fps-limit 144 --idle-work on --summary on.md
python3 tools/bench_report.py --compare off.md on.md
headless_world --view-distance 16 --hz 144 --jump 64 --idle-work off
headless_world --view-distance 16 --hz 144 --jump 64 --idle-work on
```
