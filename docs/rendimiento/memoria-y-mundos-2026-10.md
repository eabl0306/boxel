# Memoria de chunks en páginas y cambio de mundo: medición (2026-10-13)

Qué se midió: la memoria de chunks en páginas (issue #57, [ADR 0035](../adr/0035-memoria-de-chunks-en-paginas.md))
y el ciclo de vida del mundo (issue #58, M1 y M2, [ADR 0034](../adr/0034-ciclo-de-vida-del-mundo.md)),
ya juntos en main en 91e071f. M6 mide #58 M3 en su rama. La base es
90da83b, el main de antes. Solo nivel 1: cada smoke una vez, la máquina tranquila y nada
compilando. 1280x720, sin tope.

## Lo importante

- **La página queda en 32 MiB.** Crear una página dentro de un bloque de memoria que ya existe
  tarda microsegundos con 32 y con 64 MiB. Lo caro es el bloque nuevo de 256 MiB que pide el
  driver, y cuesta lo mismo con los dos tamaños. La 780M tampoco paga por los multi-draws: a
  radio 32 con páginas va un 7 % más rápida que con el arena único (M3).
- **En la 780M un bloque nuevo tarda unos 25 ms.** Pasa una vez cada 256 MiB de crecimiento y
  se ve como un frame de 25 a 31 ms. En la RTX tarda de 2 a 13 ms. Supera los 2 ms de la
  decisión 7, así que toca decidir si `MemoryHints::Manual` entra en `boxel_gfx`. Decide el dueño.
- **Crecer funciona:** de radio 8 a 32 sin chunks perdidos y sin páginas de emergencia, en las
  dos GPU (M4).
- **Encoger es más lento que lo pedido.** A los 15 s de volver a radio 8 solo se soltaron 2 de
  las 9 páginas que había creado. Es el diseño: suelta una página cada 5 s como mucho. A los
  60 s quedan 5 páginas, una sobre el piso, que la histéresis guarda a propósito (M5).
- El ciclo de vida del mundo no cambia la malla: chunks y quads iguales a 90da83b (M2).
- **Cambiar de mundo cuesta un frame de 2 a 7 ms** para cerrar y uno de 2 ms para abrir, y el
  mundo nuevo se asienta en 0,3 s a radio 16. Abrir un mundo guardado grande ya no recorre sus
  chunks en el hilo principal: 0,09 ms en vez de 13 ms a radio 32 (M6).
- **El RSS crece con los cambios**: en diez, de un 5 a un 18 % en la RTX y de un 7 a un 9 % en la
  780M según la corrida. El plan pide 5 %. Casi todo es memoria libre que malloc guarda (unos 20
  MiB); el heap en uso entre dos mundos crece unos 3 MiB y después unos 0,1 MiB por cambio, cada
  vez menos. Decide el dueño (M6).

## M1: la sonda de páginas

`just perf-smoke --base HEAD "--new-extra='--alloc-probe 32,64'" rtx/cave-static/r16 780m/cave-static/r16`.
Crea cuatro buffers de 32 MiB y cuatro de 64, uno cada medio segundo, después del present.

| GPU | 32 MiB | primer 64 MiB | otros 64 MiB | bloques |
|---|---:|---:|---:|---|
| RTX 5050 | 0,003 a 0,022 ms | 2,73 ms (frame 3,17) | 0,002 a 0,028 ms | 3 a 4, 448 a 704 MiB |
| 780M | 0,005 a 0,011 ms | 26,46 ms (frame 27,17) | 0,005 a 0,008 ms | 3 a 4, 448 a 704 MiB |
| 780M, repetida | 0,006 a 0,008 ms | 25,15 ms (frame 25,72) | 0,004 a 0,007 ms | 3 a 4, 448 a 704 MiB |

El bloque nuevo llega con el quinto buffer porque ya no queda lugar en los tres bloques, no por
su tamaño. Con páginas de 32 MiB pasa cada ocho páginas; con 64, cada cuatro.

| caso | fps | p99 ms | frames > 6,94 ms | veredicto |
|---|---:|---:|---:|---|
| rtx/cave-static/r16 | 2602 -> 2565 | 0,83 -> 0,85 | 0 -> 0 | ok |
| 780m/cave-static/r16 | 614 -> 630 | 3,71 -> 3,68 | 0 -> 1 | sale con 1 |

El 1 es el frame de la sonda que pidió el bloque. Los frames de la sonda no miden el caso.

## M2: el mundo (#58 M1)

`headless_world --world overworld --view-distance 16 --hz 0`, con y sin `--cave --camera
656,-1000,16,-90,0`: los mismos números en 90da83b y en 91e071f.

| mundo | chunks | con malla | quads |
|---|---:|---:|---:|
| superficie, r16 | 17 727 | 6 719 | 4 714 389 |
| cueva, r16 | 17 727 | 12 980 | 8 292 309 |

`just perf-smoke --base 90da83b rtx/surface/r16/uncapped 780m/surface/r16/uncapped` sale con 0.

| caso | fps | p99 ms | frames > 6,94 ms |
|---|---:|---:|---:|
| rtx/surface/r16 | 2194 -> 1958 (-10,8 %) | 0,99 -> 1,21 | 0 -> 0 |
| 780m/surface/r16 | 543 -> 520 (-4,2 %) | 4,31 -> 4,83 | 0 -> 0 |

Pasa la regla del smoke, pero son unos 0,05 ms por frame en la RTX y 0,08 en la 780M. Junta
las seis tareas (mundo, páginas e interfaz). La ronda de control dirá si se queda.

## M3: el precio de las páginas (#57 M1)

`cargo bench -p boxel_render --bench chunk_memory`, una corrida:

| bench | tiempo |
|---|---:|
| cull, 54 553 chunks en 1 página | 822 µs |
| cull, en 5 páginas como las ubica el renderer | 818 µs |
| cull, repartidos en 9 páginas (el peor caso) | 831 µs |
| ubicar 512 mallas (mezcla real) | 43,7 µs |
| flush de 512 mallas | 56,0 µs |

`just perf-smoke --base 90da83b rtx/cave-path/r32 780m/cave-path/r32` sale con 0. Los dos lados
arrancan con 48 Mi quads: la base en un arena, main en 12 páginas. Main dibuja 18 multi-draws
(9 páginas con chunks) contra 2. Ninguno pierde chunks.

| caso | fps | p99 ms | frames > 6,94 ms |
|---|---:|---:|---:|
| rtx/cave-path/r32 | 1255 -> 1241 (-1,1 %) | 2,26 -> 2,27 | 0 -> 0 |
| 780m/cave-path/r32 | 325 -> 349 (+7,3 %) | 7,73 -> 7,41 | 236 -> 119 |

## M4: crecer (#57 M2)

`just perf-smoke --base HEAD "--new-extra='--chunk-memory grow --arena-quads 8388608 --chunk-slots 32768'" rtx/grow/r32 780m/grow/r32 -- --chunk-memory fixed`
sale con 0. La base tiene 48 Mi quads fijos; el lado nuevo crece desde el piso de 8 Mi.

| caso | fps | p99 ms | frames > 6,94 ms | creación más lenta |
|---|---:|---:|---:|---:|
| rtx/grow/r32 | 1440 -> 1471 | 2,31 -> 2,25 | 0 -> 0 | 3,57 ms |
| 780m/grow/r32 | 404 -> 422 | 6,92 -> 6,52 | 35 -> 11 | 24,81 ms |

En la RTX el primer par salió peor (2 frames lentos, una creación de 13,3 ms) y la repetición no.
En las dos GPU: 9 páginas creadas, 13 al final, 0 chunks perdidos, 0 páginas de emergencia,
352 MiB de pico.

## M5: encoger (#57 M3)

`just bench-static` con la cueva quieta, `--radius-steps 8,32@1,8@20 --chunk-memory grow
--arena-quads 8388608 --chunk-slots 32768`, una vez por GPU con `--duration 35`: si a los 15 s
de volver a radio 8 quedan 4 páginas, volvió al piso.

| GPU | duración | páginas al final | soltadas | movido por frame | frame máximo |
|---|---:|---:|---:|---:|---:|
| RTX 5050 | 35 s | 11 | 2 | 0 KiB | 7,21 ms |
| 780M | 35 s | 11 | 2 | 0 KiB | 30,89 ms |
| 780M | 80 s | 5 | 8 | 0 KiB | 27,38 ms |

La de 80 s es una prueba de funcionamiento, no una medida. Nada se movió de página: las páginas
nuevas guardaban el anillo de radio 32 y se vaciaron solas. El frame máximo de la 780M es la
creación del bloque nuevo, no el encogimiento.

## M6: cambiar de mundo (#58 M3)

Rama feat/world-m3. Los tests de fugas (`crates/boxel_world/tests/lifecycle_leaks.rs`) pasan: de
30 mundos seguidos a radio 4, el heap vivo crece 315 KiB del tercero al último en la primera
corrida del binario (se llenan las cachés de cada hilo) y 3 KiB en la segunda. Los archivos
abiertos, los hilos y las slots del mapa quedan iguales.

`headless_world --world overworld --view-distance R --lod near --reopen`: un bloque cambiado en
cada chunk con bloques, así todos se guardan, y el mundo se cierra y se abre otra vez. Antes es
6b41d75, sin los índices hechos al abrir el store; después es la rama.

| radio | guardados | frame del cierre | hasta `Closed` | abrir el store | apertura antes | apertura después | asentado |
|---|---:|---:|---:|---:|---:|---:|---:|
| 16 | 8 665 | 3,92 ms | 560 ms | 1,26 -> 4,22 ms | 2,48 ms | 0,02 ms | 0,27 s |
| 32 | 32 191 | 14,36 ms | 2 048 ms | 1,72 -> 12,14 ms | 13,05 ms | 0,09 ms | 0,93 s |

Antes, de la apertura a radio 32, 0,73 ms eran el índice de guardados y 12,29 ms la semilla del
LOD. `Closed` espera el último guardado de todos los chunks editados. Abrir el store lo hace el
juego fuera del hilo principal.

`just bench-switch --world overworld --caves mc --speed 64 --radius 16 --duration 210`, una vez por
GPU, sin tope: vuela como el caso de superficie y cambia de mundo cada 20 s, diez veces.

| GPU | frame del cierre | desmontaje | hasta `Closed` | frame de la apertura | asentado | p99 fuera de las pausas | máximo fuera |
|---|---:|---:|---:|---:|---:|---:|---:|
| RTX 5050 | 2,2 a 5,0 ms | 1,0 a 2,1 ms | 9 a 12 ms | 1,6 a 2,0 ms | 0,27 a 0,29 s | 1,09 ms | 4,64 ms |
| 780M | 3,9 a 7,2 ms | 1,9 a 3,6 ms | 12 a 18 ms | 1,5 a 1,8 ms | 0,29 a 0,36 s | 4,60 ms | 8,47 ms |

La superficie en el smoke de M2 (main, 10 s) tiene p99 de 1,21 y 4,83 ms y máximo de 4,16 y 6,54
ms. El p99 fuera de las pausas queda bajo 1,5 veces el de la superficie en las dos GPU. El
máximo no, pero la superficie sin cambios tampoco cumpliría esa regla. En la 780M, 3 frames de
114 000 pasan los 6,94 ms.

Dos corridas más por GPU en 1fe1099 dan frames parecidos: p99 fuera de las pausas de 1,09 y 1,48
ms en la RTX y de 4,57 y 4,59 ms en la 780M, máximo fuera de 4,52 y 5,24 ms y de 7,26 y 8,81 ms.

El RSS crece con los cambios. Los registros de cada frame de la corrida son parte (unos 190 MiB
en la RTX), así que la corrida los resta. Desde 1fe1099, en el frame de cada apertura (el mundo
viejo cerrado, el nuevo sin cargar, una pausa declarada) la corrida lee también las cuentas de
malloc de glibc: los bytes en uso sin los registros, que una fuga hace crecer, y los bytes libres
que malloc guarda en sus arenas, residentes pero de nadie. Cuentan también lo que pide el driver
de la GPU. Leerlas tarda de 0,1 a 0,8 ms. La primera corrida de cada GPU imprimía MiB enteros.

| GPU | corrida | RSS sin los registros, 1.º -> 10.º | crece | heap en uso en la apertura | libre de malloc |
|---|---|---:|---:|---:|---:|
| RTX 5050 | anterior | 319 -> 377 MiB | 18 % | | |
| RTX 5050 | 1fe1099 | 363 -> 385 MiB | 6,1 % | 61 -> 65 MiB | 129 -> 150 MiB |
| RTX 5050 | 1fe1099 | 369 -> 386 MiB | 4,6 % | 61,4 -> 64,8 MiB | 129,7 -> 149,5 MiB |
| 780M | anterior | 278 -> 302 MiB | 9 % | | |
| 780M | 1fe1099 | 279 -> 298 MiB | 6,8 % | 39 -> 42 MiB | 120 -> 138 MiB |
| 780M | 1fe1099 | 279 -> 298 MiB | 6,8 % | 39,4 -> 42,6 MiB | 120,2 -> 138,2 MiB |
| RTX, `MALLOC_ARENA_MAX=1` | anterior | 312 -> 323 MiB | 3,5 % | | |

La última es un diagnóstico, no una medida del caso. En las corridas que leen malloc, lo que
crece es su memoria libre, unos 20 MiB: explica casi todo el RSS que crece. El heap en uso entre
dos mundos crece unos 3 MiB en los primeros cuatro o cinco cambios y después unos 0,1 MiB por
cambio, cada vez menos.
Treinta cambios cada 4 s en la RTX lo confirman: de 45,6 a 48,6 MiB hasta el 26.º, los últimos a
0,04 MiB por cambio. En esa corrida hay además un salto único de 14 MiB en el 27.º, pero una
corrida quieta sin cambios (con una traza temporal del heap cada 5 s) tiene el mismo salto a los
15 s y después queda igual en 230 000 frames: no lo causa el cambio de mundo. Una fuga de menos de
0,1 MiB por cambio no queda descartada. La regla del plan, 5 % de RSS, no se cumple en la 780M y
en la RTX depende de la corrida.

