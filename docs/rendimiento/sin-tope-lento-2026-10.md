# Corridas lentas sin tope en la RTX (2026-10)

La ronda de control 1 ([control-2026-10-09.md](control-2026-10-09.md)) bajó los fps sin tope de
la RTX en superficie (2308 -> 2155) y en la bajada (1629 -> 1547), sin marcarlos. No es el
código: algunas corridas caen en un estado lento de la GPU, con cualquiera de los dos binarios.

## Qué se ve

- Una corrida lenta va un 12 a 14 % más lenta durante toda la corrida o un tramo largo (los
  primeros 9 s, por ejemplo). Las demás corridas, de las dos versiones, se separan un 2 % como
  mucho.
- La NVIDIA marca el mismo reloj (P4, unos 2750 MHz, memoria a 9001 MHz) y 96 a 99 % de uso,
  pero consume menos: 77 a 81 W contra 83 a 91 W de las corridas normales. Trabaja más lento al
  mismo reloj.
- Con `--features trace`, el frame normal de una corrida lenta cuesta 0.03 a 0.05 ms más, todo
  dentro de `render::draw` (la espera a la GPU y el envío). Los sistemas del ECS cuestan lo mismo.
- El mundo es el mismo: las 12 corridas sin tope de superficie y bajada de la ronda terminan con
  los mismos quads y draws.

| Corrida | fps | Segundos lentos | Potencia |
|---|---:|---:|---:|
| ronda 1, superficie, antes (3) | 2304, 2313, 2306 | 0 de 20 | 86 W |
| ronda 1, superficie, ahora 1 | 2291 | 0 de 20 | 85.2 W |
| ronda 1, superficie, ahora 2 | 2043 | 19 de 20 | 80.8 W |
| ronda 1, superficie, ahora 3 | 2133 | 10 de 20 | 80.5 W |
| ronda 1, bajada, ahora 2 | 1410 | 19 de 20 | 77.3 W |
| smoke con spans, base `cf11b4f` | 2021 | 12 de 20 | 80.3 W |

Un segundo lento es uno que va más de un 8 % por debajo de la otra versión en el mismo segundo
(el recorrido de cada escenario es fijo, así que los segundos se comparan).

## El bisect

`just perf-bisect rtx/surface/r16/uncapped cf11b4f 0313bf2 --metric fps` midió 2292 -> 2292 y no
encontró nada que buscar. En la bajada, 1633 -> 1584, por una sola corrida nueva lenta (1509 fps,
11 de 20 s lentos; las otras 1623 y 1619 contra 1631, 1635 y 1633 de la base).

## Lo que no se sabe

Sobre todas las corridas sin tope de superficie y bajada (ronda, bisects y smokes), cayeron en el
estado lento 5 de 16 nuevas y 1 de 16 de la base. Con tan pocas no se distingue del azar
(Fisher p = 0.17): no se puede decir que `0313bf2` lo provoque, ni descartarlo del todo.

Para saberlo, repetir el par con más rondas ahora que el harness anota por qué baja la GPU:

```bash
just perf-bisect rtx/dive/r16/uncapped cf11b4f 0313bf2 --metric fps --rounds 6
```

## Qué cambió en `tools/perf`

- El `.nv.csv` de cada corrida suma `clocks_event_reasons.active` (la máscara de motivos por los
  que la GPU baja su reloj o su trabajo), `temperature.gpu` y `cpu_mhz` (la frecuencia media de la
  CPU). El `results.json` de la ronda guarda la media de cada corrida.
- El informe de la ronda muestra, sin tope, la corrida más lenta y la más rápida de un lado cuando
  se separan más de un 5 %. Nombra las corridas lentas, su potencia y la media sin ellas.
- `perf-bisect` mira el estado de la NVIDIA en la RTX, muestra los números de cada corrida y
  nombra las lentas. Con `--metric` rechaza `--fps-min` y `--p99-max`, que solo sirven para el
  smoke y antes se ignoraban sin aviso.

Datos en `bench-results/control/1-20261009-200903`, `bench-results/bisect/20261009-221150`,
`bench-results/bisect/20261009-222344` y `bench-results/smoke/20261009-2230*` a `-2233*` (no se
suben).
