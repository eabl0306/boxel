# Orden de los quads translúcidos dentro del chunk (2026-10-07)

Issue #38, ADR 0018 sección 3. Rama del agente `worktree-agent-af79deb18b04d753b`.

## Qué cambió

Dentro de cada chunk, los quads translúcidos (agua, vidrio) se reordenan de atrás hacia adelante
para la cámara en el pool de cómputo asíncrono, y se vuelven a subir sobre su rango del arena. Se
reordena solo cuando la cámara cruza un plano de los quads del chunk (lejos, además, cuando se
movió un octavo de la distancia).

## Cómo se midió

`stress` en la RTX 5050 (`--backend vulkan`, High, Mailbox), radio 16, sin tope de fps, 20 s por
corrida, intercaladas, con `tools/wait-idle.sh` antes de cada una. Antes: el commit 70b1653 (solo
`--water` en stress, md5 d9d73645). Después: la rama (md5 2fb91165), con el orden prendido y con
`--sort off`. Otros agentes usaban la máquina: tres corridas sueltas tuvieron frames
de 33 ms en todas las versiones (el compositor dejó de mostrar frames) y no se cuentan. El vuelo con
agua usa las vueltas 3 a 5, con la máquina quieta; el resto, las corridas que quedaron.

| Caso | Versión | fps | p99 | Peor frame | Reordenados por segundo |
|---|---|---:|---:|---:|---:|
| Vuelo a 32 con agua (`--water 62`) | antes | 2609 | 0.85 ms | 4.4 ms | 0 |
| | después | 2620 | 0.85 ms | 4.4 ms | 488 |
| | después, `--sort off` | 2625 | 0.84 ms | 4.3 ms | 0 |
| Vuelo a 32, bosque sin agua | antes (2 corridas) | 3581 | 0.71 ms | 4.3 ms | 0 |
| | después | 3573 | 0.71 ms | 4.5 ms | 0 |
| Bajada a 32 que cruza el agua (6 s) | antes (2 corridas) | 4748 | 0.72 ms | 4.1 ms | 0 |
| | después (2 corridas) | 4660 | 0.73 ms | 4.2 ms | 380 |

- **No se ve costo.** Las diferencias de fps están dentro del ruido entre corridas del mismo
  binario (unos 60 fps). Ningún frame pasó de 6.94 ms en las corridas limpias, con o sin orden.
- **Cero costo sin agua**: el bosque seco da lo mismo antes y después.
- La bajada cruza la superficie del mar: todos los chunks con agua cambian de celda a la vez y se
  reordenan en unos frames (los más cercanos primero, con topes de 64 Ki quads en vuelo y 32 Ki
  quads subidos por frame), sin tirones.

## Lo que costaba antes del ajuste

Con el mundo `--water 62` muchos chunks de mar pasan de 512 quads translúcidos (el vidrio corta la
superficie en tiras) y usan la distancia al centro. Con celdas de un bloque, o de hasta 16 bloques
lejos, el vuelo reordenaba unos 1950 buckets por segundo. Con la holgura de un octavo de la
distancia (más allá de 64 vóxeles) son unos 490 por segundo, casi todos lejos y de pocos quads.

## Costo del orden

El orden topológico es cuadrático. En el peor caso (un tablero de vidrio en 3D) tarda 0.3 ms con
512 quads y 2.4 ms con 1024 en un Ryzen 7 250, por eso el tope de 512. Un lago típico tiene de
decenas a pocos cientos de quads translúcidos por chunk.
