# Memoria de chunks en páginas y cambio de mundo: medición (2026-10-13)

Qué se midió: la memoria de chunks en páginas (issue #57, [ADR 0035](../adr/0035-memoria-de-chunks-en-paginas.md))
y el ciclo de vida del mundo (issue #58, M1 y M2), ya juntos en main en 91e071f. La base es
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
