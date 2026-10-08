# Cuevas, fase 4: ocultación de cuevas (2026-10-08)

Qué se midió: el cave culling del ADR 0022 en el overworld (`--world overworld`, cuevas `mc`),
rama del worktree `agent-a0be44c1d5c42b309`. Las corridas bajo tierra son del commit f76330b; las
de superficie del criterio, del commit 9e3e682 (antes del arreglo de los chunks sin malla, ver
abajo), salvo el vuelo de superficie de la sección de frames.

**Bajo tierra, la cámara está en una cueva.** La primera versión medía con la cámara dentro de la
roca (`--camera 16,-1000,16,...`, vuelo y bajada): `headless_world --cull` daba `open=no` en todas
esas cámaras, y con el ojo en la roca el render ve a través de ella y el culling no oculta nada
(ADR 0022, sección 5). Esos números se sacaron. Ahora `--cave` (en `stress` y `headless_world`)
busca, cerca de la cámara pedida, el vóxel de aire con 2 vóxeles de aire alrededor, 16 o más bajo
la superficie y un camino de 64 o más vóxeles así; el escenario `path` sigue ese camino. Todas las
cámaras de cueva dan `open=yes`. El vuelo viejo bajo tierra pasó 2 548 de 3 981 frames con el ojo
en la roca: el resumen de `stress` ahora avisa.

**El arreglo de los chunks sin malla** (ADR 0022, sección 2): un chunk con bloques que el render no
muestra como es (sin malla todavía, o con una malla vieja) se ve a través. Con el mundo cargado no
cambia nada (los conteos de superficie de abajo dan igual). Mientras el mundo carga o se publica
una edición, oculta algo menos.

**La máquina estaba compartida** (carga de 1.4 a 4.4). `tools/wait-idle.sh` dejó pasar todas las
corridas, pero los tiempos son orientativos: el dueño los vuelve a medir con los comandos del
final. Los conteos de draws y caras no dependen de la carga.

## Resultado

| Criterio | Objetivo | Medido | |
|---|---:|---:|---|
| Puerta del plan: caras subterráneas ocultas desde la superficie, radio 16 | 70 % o más | 15 a 45 % (27 % de media) | no pasa |
| 780M radio 16 en una cueva, quieta y girando: frames > 6.94 ms | 0 | 0 en las 4 corridas | pasa |
| 780M radio 16 en una cueva, camino a 8 vóxeles/s: frames > 6.94 ms | 0 | 17 y 5 (máximo 7.9 ms, GPU) | casi |
| Búsqueda a radio 16 | 0.3 ms o menos | en una cueva 0.06 a 0.09 ms; superficie 0.27 a 0.37 ms | pasa |
| Búsqueda a radio 32 | 1 ms o menos | en una cueva 0.16 ms (p99 0.45); superficie 0.8 ms (p99 1.3) | casi |
| `face_info` por chunk | 15 µs o menos | 6 a 7 µs en cuevas, 11 µs en superficie | pasa |

Lo importante:

- **En una cueva funciona:** se dibuja de 1.8 a 4.4 veces menos (44 a 77 % de las caras ocultas,
  61 % de media en 9 cuevas). En la 780M a radio 16 pasa de unos 300 fps a entre 500 y 800, y de
  cientos de frames lentos a ninguno quieta o girando. Caminando por la cueva quedan 5 a 17 frames
  apenas por encima de 6.94 ms, todos de GPU, mirando cavernas grandes que sí se ven.
- **En la superficie no alcanza la puerta del plan.** Es la fuga de los chunks de 32³ que el plan
  temía: en un chunk de superficie, el aire del valle y una cueva tocan la misma cara, y el enlace
  por par de caras los une. Con cuevas cerradas (`--caves closed`) oculta 20 %: no son las
  entradas. Lo siguiente es la fase 5: enlaces por subceldas de 16³, o Hi-Z en High.
- Las búsquedas cuestan poco: en una cueva 0.06 a 0.09 ms. Volando se busca en 1 a 5 % de los
  frames (ADR 0022, sección 4); al girar o caminar mirando a otro lado, en casi todos.
- Las ediciones casi no cambian lo oculto: con 20 ediciones en 20 s junto a la cámara (`--edits`)
  quedan 11 559 chunks ocultos de media, contra 11 604 sin ediciones.

## Draws y caras (deterministas)

`headless_world --cull`: lo que dibujaría cada tier (frustum y test de cara por bucket, como
`cull.wgsl`) sin y con cave culling, desde una cámara de 16:9 con distancia de vista = radio x 32.
"Buckets" son los draws de Mid y Low. "Bajo tierra" son los chunks que quedan enteros bajo la
superficie (en 5 columnas de muestra). Radio 16, radio vertical 8.

| Cámara | Quads sin | Quads con | Ocultos | Buckets sin / con | Bajo tierra ocultos |
|---|---:|---:|---:|---:|---:|
| Superficie (16, 140, 16), 20 grados abajo | 679 302 | 622 800 | 8.3 % | 5 068 / 4 297 | 14.7 % |
| Superficie, x = 336 | 717 095 | 626 149 | 12.7 % | 5 273 / 4 238 | 20.7 % |
| Superficie, x = 656 | 707 848 | 521 317 | 26.4 % | 5 245 / 3 297 | 45.3 % |
| Superficie, x = 976 | 635 368 | 535 872 | 15.7 % | 4 952 / 3 669 | 26.3 % |
| Superficie, x = 1296 | 671 589 | 563 000 | 16.2 % | 5 101 / 3 931 | 26.7 % |
| Superficie, cuevas `closed` | 655 871 | 581 320 | 11.4 % | 5 054 / 4 133 | 20.0 % |
| `forest`, superficie | 263 115 | 263 110 | 0.0 % | 1 490 / 1 489 | - |
| Radio 32, superficie | 2 731 252 | 2 471 860 | 9.5 % | 20 200 / 16 819 | 15.8 % |

Bajo tierra, con el ojo en una cueva (`--cave`, mirando a +x): la cueva más cercana a cada cámara
vieja de vuelo (y = -1000) y de bajada (x = 16). Todas dan `open=yes`. Todo es bajo tierra.

| Cerca de | Ojo | Camino (vóxeles, bordes de chunk) | Quads sin | Quads con | Ocultos | Buckets sin / con |
|---|---|---:|---:|---:|---:|---:|
| (16, -1000, 16) | (22.5, -976.5, 46.5) | 97, 4 | 1 059 509 | 463 927 | 56.2 % | 9 221 / 3 808 |
| (336, -1000, 16) | (346.5, -1001.5, 19.5) | 160, 7 | 967 175 | 338 246 | 65.0 % | 9 007 / 2 912 |
| (656, -1000, 16) | (648.5, -1014.5, 5.5) | 304, 10 | 1 087 293 | 506 883 | 53.4 % | 9 745 / 4 007 |
| (976, -1000, 16) | (940.5, -979.5, 39.5) | 157, 4 | 1 071 140 | 459 007 | 57.1 % | 9 529 / 3 701 |
| (1296, -1000, 16) | (1286.5, -1014.5, 38.5) | 69, 2 | 1 109 942 | 268 228 | 75.8 % | 9 721 / 2 058 |
| (16, -1320, 16) | (36.5, -1329.5, 9.5) | 231, 6 | 1 169 111 | 656 845 | 43.8 % | 9 997 / 5 162 |
| (16, -1640, 16) | (12.5, -1631.5, 10.5) | 362, 10 | 1 154 409 | 562 662 | 51.3 % | 9 699 / 4 251 |
| (16, -1960, 16) | (2.5, -1982.5, 7.5) | 109, 5 | 1 093 322 | 348 409 | 68.1 % | 9 894 / 2 965 |
| (16, -2280, 16) | (0.5, -2271.5, -1.5) | 242, 5 | 1 121 016 | 253 147 | 77.4 % | 10 028 / 2 082 |
| Radio 32, (16, -1000, 16) | (22.5, -976.5, 46.5) | 97, 4 | 4 792 345 | 1 704 183 | 64.4 % | 42 909 / 14 294 |

Las corridas con ventana de abajo usan la cueva cerca de (656, -1000, 16). A radio 32 los buckets
bajan de 42 909 a 14 294, lejos del tope de 65 535 draws de High.

Sin frustum en la búsqueda (para buscar solo al cambiar de chunk) se oculta menos: en las cinco
cuevas de y = -1000, 44 a 76 % en vez de 53 a 76 %. Por eso la búsqueda usa el frustum.

## Costo de la búsqueda y de los enlaces

Criterion (`cargo bench -p boxel_world --bench cave_culling`), con la máquina compartida:

| Bench | Caché caliente | Caché fría |
|---|---:|---:|
| `cave_bfs/r16_surface_search` | 65 a 75 µs | 107 µs |
| `cave_bfs/r16_deep_search` | 22 a 35 µs | 87 µs |
| `cave_bfs/r32_surface_search` | 317 a 338 µs | 580 µs |
| `cave_bfs/r32_deep_search` | 75 a 137 µs | 244 µs |
| `cave_bfs/r16_surface_cross` (cambio de chunk) | 76 µs | - |
| `cave_bfs/r32_surface_cross` | 379 µs | - |
| `face_info/cave_chunk` | 6.4 a 7.1 µs | - |
| `face_info/surface_chunk` | 11.1 a 11.4 µs | - |
| `face_info/deep_chunks_mean_x64` | 6.0 µs por chunk | - |
| `generate/cave_chunk` (referencia) | 30 a 33 µs | - |
| `generate/surface_chunk` (referencia) | 94 a 98 µs | - |

En el juego (`stress`, sección "Cave culling" del resumen), por búsqueda. Las de superficie con
9e3e682 son de antes del arreglo de los chunks sin malla.

| Corrida | Búsquedas | Media | p99 | Máximo |
|---|---:|---:|---:|---:|
| 780M r16 en una cueva, giro | 15 704 de 15 705 frames | 0.058 ms | 0.17 ms | 0.51 ms |
| 780M r16 en una cueva, camino a 8 | 9 066 de 9 973 | 0.086 ms | 0.25 ms | 0.74 ms |
| 780M r16 en una cueva, quieta con `--edits` | 38 de 11 179 | 0.30 ms | 0.59 ms | 0.59 ms |
| RTX r32 en una cueva, camino a 8 | 25 785 de 28 026 | 0.16 ms | 0.45 ms | 1.18 ms |
| 780M r16 superficie, vuelo 64 (f76330b) | 516 de 10 487 | 0.27 ms | 0.44 ms | 0.58 ms |
| 780M r16 superficie, vuelo 64 (9e3e682) | 385 de 9 470 | 0.37 ms | 0.71 ms | 0.85 ms |
| 780M r16 superficie, giro (9e3e682) | 9 894 de 9 895 | 0.15 ms | 0.34 ms | 0.77 ms |
| RTX r32 superficie, vuelo 64 (9e3e682) | 384 de 12 920 | 0.81 ms | 1.33 ms | 1.94 ms |

Los enlaces cuestan de 12 a 20 % del job de generación, que es menos de un tercio del trabajo de
un chunk (malla y luz cuestan más): alrededor de 5 % del total. Se pagan también con el culling
apagado (ADR 0022, sección 2).

## Frames con ventana

`stress`, 20 s, sin límite de fps. En una cueva: `--cave --camera 656,-1000,16,-90,0` (el ojo en
(648.5, -1014.5, 5.5), camino de 304 vóxeles), commit f76330b, 2 vueltas intercaladas en la 780M
(la segunda entre paréntesis). Superficie: `16,140,16,-90,-20`.

| Caso | Culling | fps | p99 | > 6.94 ms |
|---|---|---:|---:|---:|
| 780M r16 cueva, quieta | sí | 544 (549) | 4.76 (4.56) ms | 0 (0) |
| | no | 280 (293) | 8.39 (7.61) ms | 757 (375) |
| 780M r16 cueva, giro | sí | 785 (803) | 4.25 (4.19) ms | 0 (0) |
| | no | 295 (321) | 8.13 (7.29) ms | 518 (181) |
| 780M r16 cueva, camino a 8 | sí | 499 (505) | 5.89 (5.91) ms | 17 (5) |
| | no | 310 (330) | 7.59 (7.28) ms | 313 (159) |
| 780M r16 cueva, quieta con `--edits` | sí | 559 | 4.44 ms | 1 |
| | no | 302 | 7.41 ms | 277 |
| RTX r32 cueva, camino a 8 | sí | 1 401 | 2.58 ms | 0 |
| | no | 623 | 4.26 ms | 0 |
| RTX r32 cueva, quieta | sí | 1 318 | 2.01 ms | 0 |
| | no | 505 | 4.86 ms | 0 |
| 780M r16 superficie, vuelo 64 (f76330b) | sí | 524 | 5.11 ms | 1 |
| | no | 493 | 5.02 ms | 2 |
| 780M r16 superficie, vuelo 64 (9e3e682) | sí | 473 (543) | 6.28 (4.79) ms | 42 (7) |
| | no | 480 (484) | 5.25 (5.23) ms | 2 (0) |
| 780M r16 superficie, giro (9e3e682) | sí | 495 | 5.64 ms | 10 |
| | no | 476 | 5.39 ms | 4 |
| RTX r32 superficie, vuelo 64 (9e3e682) | sí | 646 | 4.31 ms | 3 |
| | no | 593 | 4.34 ms | 8 |

- En una cueva la diferencia es grande y se repite: casi el doble de fps y ningún frame lento
  quieta o girando. Los frames lentos que quedan caminando no hacen trabajo de mundo ni buscan:
  esperan a la GPU mirando cavernas grandes.
- En la superficie las dos versiones quedan dentro del ruido de la máquina ocupada.
- El arreglo de los chunks sin malla no se nota en el vuelo de superficie (524 contra 493 fps).
- El vuelo viejo bajo tierra (`--camera 16,-1000,16,-90,0`, sin `--cave`) ahora da 398 fps y avisa:
  el ojo pasó 2 548 de 3 981 frames en la roca. Los 2 300 fps de la primera versión salían de
  ocultar cosas que se veían.

## Cómo volver a medir (para el dueño)

Todo en release, con la máquina quieta, intercalando con y sin culling, 3 vueltas o más. **Bajo
tierra, siempre con `--cave`:** sin él la cámara puede estar en la roca, donde no se oculta nada
(`stress` lo avisa en el resumen). `fly` y `dive` atraviesan la roca: `stress` no los acepta con
`--cave`; para moverse por una cueva está `path`.

```bash
# Conteos deterministas (no hace falta máquina quieta)
cargo build --release --locked --example headless_world
HW=target/release/examples/headless_world
for c in 16,140,16,-90,-20 336,140,16,-90,-20 656,140,16,-90,-20; do
  $HW --world overworld --view-distance 16 --hz 0 --cull --camera $c
done
for c in 16,-1000,16,-90,0 656,-1000,16,-90,0 16,-1640,16,-90,0 16,-2280,16,-90,0; do
  $HW --world overworld --view-distance 16 --hz 0 --cull --cave --camera $c   # debe decir open=yes
done
$HW --world overworld --view-distance 32 --hz 0 --cull --camera 16,140,16,-90,-20 --timeout 300
$HW --world overworld --view-distance 32 --hz 0 --cull --cave --camera 16,-1000,16,-90,0 --timeout 300

# Benches de la búsqueda y de los enlaces
tools/wait-idle.sh 60 && cargo bench -p boxel_world --bench cave_culling

# Frames con ventana (cada receta espera a que la CPU se calme y marca -busy si no)
CAVE="--world overworld --cave --camera 656,-1000,16,-90,0"
for round in 1 2 3; do
  for c in on off; do
    just bench-static $CAVE --adapter 780m --cave-culling $c
    just bench-spin   $CAVE --adapter 780m --cave-culling $c
    just bench-path $CAVE --speed 8 --adapter 780m --cave-culling $c
    just bench-path $CAVE --speed 8 --radius 32 --adapter nvidia --cave-culling $c
    just bench-fly  --world overworld --speed 64 --camera 16,140,16,-90,-20 --adapter 780m --cave-culling $c
    just bench-spin --world overworld --camera 16,140,16,-90,-20 --adapter 780m --cave-culling $c
    just bench-fly  --world overworld --speed 64 --radius 32 --camera 16,140,16,-90,-20 --adapter nvidia --cave-culling $c
  done
done
# Con el límite del juego: ¿la búsqueda hace perder refrescos? (presupuesto del mundo, ADR 0022)
just bench-fly  --world overworld --speed 64 --radius 32 --camera 16,140,16,-90,-20 --adapter nvidia --fps-limit 144
just bench-path $CAVE --speed 8 --radius 32 --adapter nvidia --fps-limit 144
just bench-report --compare <corrida-off>.md <corrida-on>.md
```

`tools/bench_report.py` empareja una corrida con culling con las de main de antes (no separa por
culling) y anota la diferencia; compara además los chunks ocultos, los draws y el tiempo de
búsqueda. Las corridas con `--cave` solo se emparejan entre ellas (el título termina en `cave`).
