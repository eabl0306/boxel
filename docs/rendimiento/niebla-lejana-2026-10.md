# Bruma lejana y nubes grandes: medición del M1 y del M2 (2026-10-11 y 2026-10-12)

Qué se midió: el M1 de la ronda de niebla ([ADR 0032](../adr/0032-bruma-lejana-y-nubes-grandes.md),
issue #47), rama `feat/fog-far` en 4f43b4d. S4 se midió tras el vuelo del dueño, en 7ae1be8,
con el mismo binario (desde 4f43b4d solo cambiaron docs). El M2 (la optimización de la marcha) se
midió el 2026-10-12, de S5 a S8. Solo nivel 1: cada smoke una vez, con su `--base`, la máquina
tranquila y nada compilando. RTX 5050, 1280x720, radio 16, sin tope. Costo = el frame medio con
niebla menos sin niebla (por 1/fps), y la diferencia de medianas.

## Lo importante del M2

- **El M2 no llega a la meta.** `both` cuesta ahora 1,06 a 1,11 ms por frame medio (S8), contra
  1,17 a 1,22 en el punto de partida (S0): unas 1,1 veces menos, lejos de 1,5. En la mediana
  casi no cambió (0,96 a 1,01 ms contra 0,97 a 1,05). Por debajo de 1,4 la regla es parar e
  informar: no se empezó la reserva (especializar el shader o bajar `EXIT_TAU`). Decide el dueño.
- Lo que sí bajó, medido contra el paso anterior: los atajos exactos (M2a-1) 0,10 a 0,12 ms por
  frame (S5); caminar solo las filas que pueden tener niebla (M2b) otros 0,06 a 0,07 (S7), y
  0,18 ms con la cámara dentro de `inside`, donde los frames de más de 6,94 ms bajaron de 53 a 31.
- Reusar la cadena de hashes (E2) hizo la niebla más lenta: 0,24 a 0,26 ms más por frame (S6).
  Se revirtió.
- Mirando al horizonte con el LOD, `decks` pasó de 1,52 a 1,36 ms por frame (S3 contra S8).
- Entre sesiones el mismo binario varía unos 0,07 ms en la mediana, así que la comparación con
  S0 (otra sesión) tiene ese ruido.

## Lo importante del M1

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

## S5 a S8: la optimización (M2)

Los pasos del plan, en orden, cada uno con su commit y un smoke. Los de S5 a S7 comparan dos
binarios, los dos con `--fog both` (o `inside`): la columna de cambio es la del frame medio. El
harness repitió algunos pares; las tablas muestran el último.

**S5, M2a-1 (0e0b830) contra el M1 (7ae1be8).** Los atajos exactos: el umbral de cada fila una
vez por fila, la segunda octava sin interpolar otra vez la primera, la niebla suave que decide
con la primera octava, una celda sin cobertura posible que no se interpola, menos cuentas por
paso. Las imágenes son las mismas byte a byte. Sale con 0.

| caso | fps | p50 ms | p99 ms | frames > 6,94 ms | cambio |
|---|---:|---:|---:|---:|---:|
| superficie | 590 -> 635 | 1,52 -> 1,41 | 4,38 -> 4,20 | 1 -> 0 | -0,120 ms |
| bosque | 645 -> 688 | 1,39 -> 1,32 | 4,27 -> 4,05 | 0 -> 0 | -0,097 ms |

**S6, M2a-2 (47fe21c) contra M2a-1.** Reusar la cadena de hashes del ruido al pasar a un paso
vecino de la red: 4 a 7 hashes en lugar de 14, las mismas imágenes. Pero guarda unos 12 valores
más por octava, y el shader se hizo más lento. Se revirtió (df46e43): la regla pedía un 3 % menos
de mediana en los dos casos.

| caso | fps | p50 ms | p99 ms | frames > 6,94 ms | cambio |
|---|---:|---:|---:|---:|---:|
| superficie | 633 -> 543 | 1,41 -> 1,63 | 4,17 -> 4,79 | 1 -> 0 | +0,262 ms |
| bosque | 691 -> 593 | 1,32 -> 1,51 | 3,98 -> 4,46 | 0 -> 0 | +0,239 ms |

**S7, M2b (1df8ee5) contra M2a (0e0b830, el mismo código que df46e43).** La marcha ya no camina
la fila pegada a un plano con borde, que siempre está vacía, y multiplica por recíprocos
calculados en la CPU. Cambian los pasos y el redondeo, dentro de las tolerancias de los tests de
GPU. Sale con 0. Tras la revisión, con niebla suave y un borde de menos de dos celdas la fila se
vuelve a caminar (su niebla suave se ve). Ningún preset tiene un borde así, así que estos números
no cambian.

| caso | fps | p50 ms | p99 ms | frames > 6,94 ms | cambio |
|---|---:|---:|---:|---:|---:|
| superficie, `both` | 628 -> 655 | 1,42 -> 1,38 | 4,26 -> 4,32 | 1 -> 0 | -0,066 ms |
| bosque, `both` | 707 -> 739 | 1,28 -> 1,24 | 3,94 -> 3,67 | 0 -> 0 | -0,061 ms |
| superficie, `inside` | 313 -> 332 | 2,99 -> 2,78 | 7,01 -> 6,94 | 53 -> 31 | -0,183 ms |

**S8, final, el mismo binario (1df8ee5) sin niebla y con ella.** Sale con 1 por la regla del
85 % de fps: el precio del plugin, como en S0.

| caso | fps | p50 ms | p99 ms | costo medio | costo p50 | en S0 o S3 |
|---|---:|---:|---:|---:|---:|---:|
| superficie, `both` | 2165 -> 656 | 0,42 -> 1,38 | 1,05 -> 4,12 | 1,062 ms | 0,96 ms | 1,173 y 0,97 ms |
| bosque, `both` | 3857 -> 728 | 0,23 -> 1,24 | 0,71 -> 3,75 | 1,114 ms | 1,01 ms | 1,221 y 1,05 ms |
| `decks`, horizonte, `--lod far` | 1158 -> 450 | 0,76 -> 2,00 | 2,19 -> 5,35 | 1,359 ms | 1,24 ms | 1,523 y 1,41 ms |

- Contra S0 (0bd2849), la niebla `both` cuesta 1,10 veces menos por frame medio en los dos
  casos y de 1,01 a 1,04 veces menos en la mediana. La meta era 1,5.
- `decks` (S3, en 4f43b4d) bajó 1,12 veces por frame medio y 1,14 en la mediana.
- Por qué rinde menos que lo estimado: el plan contaba operaciones por paso, pero lo más probable
  es que el shader dependa más de los registros que de esas cuentas. E2, que ahorra hashes, costó
  un 14 % de fps. Las perillas que quedan son la reserva del plan (especializar el shader por capa
  con `override`, `EXIT_TAU` en 8), que espera el sí del dueño, y las del juego (octavas,
  densidad, pasos, `soft_distance`).

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

Después del M2 (1df8ee5): sin caminar la fila pegada a cada plano con borde, los rayos rasantes
dan menos pasos y casi ninguno agota el presupuesto (`both`: de 16,2 a 11,9 pasos medios en la
pose `stress`, de 20,9 a 16,2 en `horizon`, y de 2 % a 0 % de rayos que agotan el tope).

| preset | pose | pasos medios | p90 | máx | tope agotado | trozos suaves | trozos de bruma |
|---|---|---:|---:|---:|---:|---:|---:|
| both | stress | 11,9 | 34 | 62 | 0 % | 0,0 | 0,0 |
| both | horizon | 16,2 | 34 | 62 | 0 % | 0,0 | 0,0 |
| both | above | 9,4 | 21 | 29 | 0 % | 0,0 | 0,0 |
| both | deck | 14,0 | 40 | 64 | 3 % | 0,0 | 0,1 |
| cumulus16 | stress | 15,4 | 47 | 64 | 4 % | 0,0 | 0,4 |
| cumulus16 | horizon | 19,7 | 47 | 64 | 4 % | 0,0 | 0,4 |
| cumulus16 | above | 20,6 | 49 | 64 | 6 % | 0,0 | 1,4 |
| cumulus16 | deck | 9,9 | 15 | 24 | 0 % | 53,0 | 0,0 |
| cumulus20 | stress | 15,2 | 44 | 64 | 4 % | 0,0 | 0,3 |
| cumulus20 | horizon | 21,2 | 44 | 64 | 4 % | 0,0 | 0,3 |
| cumulus20 | above | 20,5 | 46 | 64 | 7 % | 0,0 | 1,4 |
| cumulus20 | deck | 24,4 | 52 | 64 | 7 % | 48,5 | 0,8 |
| cumulus24 | stress | 11,8 | 33 | 64 | 2 % | 0,0 | 0,3 |
| cumulus24 | horizon | 16,3 | 33 | 64 | 2 % | 3,1 | 0,3 |
| cumulus24 | above | 16,9 | 38 | 64 | 3 % | 0,0 | 1,0 |
| cumulus24 | deck | 17,7 | 31 | 64 | 1 % | 49,3 | 0,5 |
| decks | stress | 16,1 | 50 | 64 | 4 % | 0,0 | 0,6 |
| decks | horizon | 21,5 | 50 | 64 | 4 % | 0,0 | 0,6 |
| decks | above | 25,3 | 53 | 64 | 7 % | 32,1 | 1,4 |
| decks | deck | 11,8 | 21 | 32 | 0 % | 53,0 | 0,0 |
| inside | stress | 64,0 | 64 | 64 | 100 % | 27,3 | 1,6 |
| inside | horizon | 64,0 | 64 | 64 | 100 % | 27,2 | 1,7 |
| inside | above | 31,2 | 64 | 64 | 46 % | 0,0 | 1,3 |
| inside | deck | 35,5 | 64 | 64 | 46 % | 21,5 | 0,8 |

## Archivos

En `bench-results/fog-far/` (fuera de git): `S0`, `S1`, `S2`, `S3-*` y `S4/` (una carpeta por
preset y pose) con el log de cada smoke, sus corridas (`runs/`) y todas las métricas
(`compare.txt`, del primer par), y `M1/` con la tabla de pasos y los logs de los builds y del
arranque de `voxel_world`. Del M2: `S5` a `S8` igual, y `M2/` con la tabla de pasos y las
capturas que comparan cada paso (`M2/shots/`, con `shots.sh`).
