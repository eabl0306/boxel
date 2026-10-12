# Bruma lejana y nubes grandes: medición del M1 (2026-10-11, S4 el 2026-10-12)

Qué se midió: el M1 de la ronda de niebla ([ADR 0032](../adr/0032-bruma-lejana-y-nubes-grandes.md),
issue #47), rama `feat/fog-far` en 4f43b4d. S4 se midió tras el vuelo del dueño, en 7ae1be8,
con el mismo binario (desde 4f43b4d solo cambiaron docs). Solo nivel 1: cada smoke una vez, con
su `--base`, la máquina tranquila y nada compilando. RTX 5050, 1280x720, radio 16, sin tope.
Costo = el frame medio con niebla menos sin niebla (por 1/fps), y la diferencia de medianas.

## Lo importante

- `both` se ve igual que antes y cuesta casi lo mismo: el M1 le suma unos 0,08 ms por frame
  (S1) y el smoke pasa. Es del tamaño de lo que varía el mismo binario entre dos sesiones (su
  mediana dio 1,40 ms en S0 y 1,47 en S1), así que puede ser ruido. El M2 parte de aquí.
- La bruma lejana con el LOD cuesta 0,03 ms por frame (S2), lo estimado.
- Mirando al horizonte con el LOD lejano, los cuatro presets nuevos cuestan de 1,43 a 1,52 ms por
  frame (S3). Entre ellos hay menos de 0,1 ms; tras el vuelo, el dueño se quedó con todos. A
  1080p el costo por texel es unas 2,25 veces mayor.
- Volar dentro de una nube es lo más caro: de 2,24 a 2,30 ms por frame con `decks` o `cumulus16`
  (S4), por la niebla suave. Desde arriba, `cumulus16` cuesta 1,36 ms. Dentro de una nube el p99
  llega a 6,9 ms a 720p: es lo que más tiene que bajar el M2.
- S3 y S4 salen con 1 por la regla del 85 % de fps: es el precio del plugin, como en S0 y en
  ADR 0030, no una regresión.

## S0: punto de partida

En 0bd2849, el mismo binario sin niebla y con `--fog both`: 0,97 a 1,06 ms en la mediana y 1,17
a 1,22 ms en el frame medio (superficie y bosque). La meta del M2 es llevar la mediana a unos
0,65 a 0,7 ms.

## S1: el M1 contra el punto de partida

`perf.py smoke --base 0bd2849 surface forest -- --fog both`: el binario de 0bd2849 contra el de
4f43b4d, los dos con `--fog both`. Sale con 0.

| caso | fps | p50 ms | p99 ms | frames > 6,94 ms | costo medio | costo p50 |
|---|---:|---:|---:|---:|---:|---:|
| superficie | 616 -> 588 | 1,47 -> 1,54 | 4,48 -> 4,60 | 0 -> 0 | +0,077 ms | +0,07 ms |
| bosque | 683 -> 646 | 1,33 -> 1,41 | 4,34 -> 4,36 | 0 -> 0 | +0,084 ms | +0,08 ms |

## S2: lo que cuesta la bruma

`perf.py smoke --base HEAD --new-extra '--fog-haze 1' surface -- --lod far --fog cumulus16
--fog-haze 0`: el mismo binario y las mismas celdas. La base no tiene bruma (las nubes se
desvanecen en sus celdas); el lado nuevo tiene bruma 1 en el valle y en las nubes. Sale con 0.

| caso | fps | p50 ms | p99 ms | frames > 6,94 ms | costo medio | costo p50 |
|---|---:|---:|---:|---:|---:|---:|
| superficie, `--lod far`, `cumulus16` | 438 -> 432 | 2,07 -> 2,12 | 5,47 -> 5,56 | 1 -> 2 | +0,032 ms | +0,05 ms |

## S3: cada preset mirando al horizonte

`perf.py smoke --base HEAD --new-extra '--fog X' surface -- --lod far --camera
16,140,16,-90,8`: el mismo binario sin niebla y con cada preset, en la pose del horizonte (y 140,
8 grados arriba). El harness repitió cada par y los dos dieron lo mismo; la tabla muestra el
segundo.

| preset | fps | p50 ms | p99 ms | frames > 6,94 ms | costo medio | costo p50 | estimado |
|---|---:|---:|---:|---:|---:|---:|---:|
| `cumulus16` | 1147 -> 430 | 0,77 -> 2,13 | 2,23 -> 5,53 | 0 -> 1 | 1,45 ms | 1,36 ms | 1,2 a 1,9 |
| `cumulus20` | 1146 -> 435 | 0,77 -> 2,09 | 2,20 -> 5,51 | 0 -> 3 | 1,43 ms | 1,32 ms | 1,1 a 1,7 |
| `cumulus24` | 1158 -> 432 | 0,77 -> 2,12 | 2,18 -> 5,45 | 0 -> 2 | 1,45 ms | 1,35 ms | 0,8 a 1,3 |
| `decks` | 1143 -> 417 | 0,77 -> 2,18 | 2,25 -> 5,74 | 0 -> 3 | 1,52 ms | 1,41 ms | 1,4 a 2,2 |

`cumulus24` no salió más barato que los otros, como se esperaba. Lo más probable es su banda: tras
el ajuste empieza en 144, justo sobre la pose, y los rayos que suben cruzan su niebla suave (22,4
trozos suaves por rayo en la tabla de pasos, contra 0,3 de `cumulus16`). Las perillas son del
juego (el piso de la banda, `soft_distance`). No se ajustó nada.

## S4: los presets más caros, desde adentro y desde arriba

El dueño se queda con todos los presets, así que se midieron los más caros según la tabla de
pasos, `decks` y `cumulus16`, en dos poses: dentro de la capa (y 250, a nivel) y arriba (y 420, 25
grados abajo). `perf.py smoke --base HEAD --new-extra '--fog X' surface -- --lod far --camera
POSE`: el mismo binario sin niebla y con el preset. El harness repitió cada par y los dos dieron
lo mismo; la tabla muestra el segundo.

| preset | pose | fps | p50 ms | p99 ms | frames > 6,94 ms | costo medio | costo p50 |
|---|---|---:|---:|---:|---:|---:|---:|
| `decks` | dentro (y 250) | 1282 -> 325 | 0,70 -> 2,83 | 2,00 -> 6,91 | 0 -> 27 | 2,30 ms | 2,13 ms |
| `decks` | arriba (y 420) | 1571 -> 348 | 0,57 -> 2,68 | 1,70 -> 6,40 | 0 -> 1 | 2,24 ms | 2,11 ms |
| `cumulus16` | dentro (y 250) | 1262 -> 330 | 0,70 -> 2,81 | 2,05 -> 6,87 | 0 -> 22 | 2,24 ms | 2,11 ms |
| `cumulus16` | arriba (y 420) | 1580 -> 502 | 0,57 -> 1,83 | 1,70 -> 4,82 | 0 -> 0 | 1,36 ms | 1,26 ms |

- Dentro de una nube cuesta 1,5 veces lo de la pose del horizonte (S3). Es la niebla suave: unos
  53 trozos suaves por rayo, que no cuentan en el presupuesto de pasos.
- Para `decks`, y 420 no es arriba: cae dentro de su capa alta (400 a 440), con 40 trozos suaves
  por rayo, y cuesta lo mismo que dentro de los cúmulos. `cumulus16` desde arriba queda en lo
  estimado (0,9 a 1,5 ms).
- Dentro de una nube, a 720p, el p99 llega a 6,9 ms y de 22 a 27 frames de 10 s pasan de 6,94 ms
  (144 Hz). A 1080p el costo por texel sube unas 2,25 veces. Es el caso que más tiene que bajar
  el M2: su E4 (decidir con la primera octava en los trozos suaves) apunta ahí.
- Los cuatro salen con 1 por la regla del 85 % de fps: el precio del plugin. No se ajustó nada;
  las perillas son del juego (`soft_distance`, octavas, densidad).

## Pasos por rayo

`cargo test -p boxel_demo fog_steps_per_ray -- --nocapture` en 4f43b4d: la marcha de referencia
sobre un abanico fijo de 360 rayos por pose, con el horizonte `far` y una vista de 512. Poses:
`stress` (y 140, 20 grados abajo), `horizon` (y 140, 8 arriba), `above` (y 420, 25 abajo) y
`deck` (y 250, nivel). Trozos por rayo, en promedio.

| preset | pose | pasos medios | p90 | máx | tope agotado | trozos suaves | trozos de bruma |
|---|---|---:|---:|---:|---:|---:|---:|
| both | stress | 16,2 | 46 | 64 | 2 % | 0,0 | 0,0 |
| both | horizon | 20,9 | 46 | 64 | 2 % | 0,0 | 0,0 |
| both | above | 11,9 | 27 | 40 | 0 % | 0,0 | 0,0 |
| both | deck | 17,8 | 54 | 64 | 6 % | 0,5 | 0,1 |
| cumulus16 | stress | 19,5 | 64 | 64 | 10 % | 0,0 | 0,4 |
| cumulus16 | horizon | 24,8 | 64 | 64 | 10 % | 0,3 | 0,4 |
| cumulus16 | above | 25,1 | 63 | 64 | 9 % | 0,0 | 1,5 |
| cumulus16 | deck | 11,3 | 20 | 30 | 0 % | 53,0 | 0,0 |
| cumulus20 | stress | 19,1 | 53 | 64 | 7 % | 0,0 | 0,4 |
| cumulus20 | horizon | 25,8 | 53 | 64 | 7 % | 6,7 | 0,4 |
| cumulus20 | above | 23,3 | 54 | 64 | 8 % | 0,0 | 1,5 |
| cumulus20 | deck | 27,3 | 57 | 64 | 8 % | 48,5 | 0,8 |
| cumulus24 | stress | 16,3 | 46 | 64 | 5 % | 7,2 | 0,3 |
| cumulus24 | horizon | 22,0 | 46 | 64 | 5 % | 22,4 | 0,3 |
| cumulus24 | above | 21,4 | 50 | 64 | 6 % | 0,0 | 1,1 |
| cumulus24 | deck | 22,0 | 38 | 64 | 2 % | 49,3 | 0,5 |
| decks | stress | 20,5 | 64 | 64 | 12 % | 0,0 | 0,9 |
| decks | horizon | 27,5 | 64 | 64 | 12 % | 0,3 | 0,9 |
| decks | above | 32,6 | 64 | 64 | 11 % | 40,1 | 1,6 |
| decks | deck | 14,4 | 29 | 42 | 0 % | 53,0 | 0,0 |
| inside | stress | 64,0 | 64 | 64 | 100 % | 27,3 | 1,6 |
| inside | horizon | 64,0 | 64 | 64 | 100 % | 27,2 | 1,7 |
| inside | above | 31,5 | 64 | 64 | 47 % | 0,0 | 1,3 |
| inside | deck | 38,6 | 64 | 64 | 47 % | 24,1 | 0,8 |

## Archivos

En `bench-results/fog-far/` (fuera de git): `S0`, `S1`, `S2`, `S3-*` y `S4/` (una carpeta por
preset y pose) con el log de cada smoke, sus corridas (`runs/`) y todas las métricas
(`compare.txt`, del primer par), y `M1/` con la tabla de pasos y los logs de los builds y del
arranque de `voxel_world`.
