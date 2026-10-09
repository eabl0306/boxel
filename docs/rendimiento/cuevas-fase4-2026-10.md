# Cuevas, fase 4: ocultación de cuevas (2026-10-08)

Qué se midió: el cave culling del ADR 0022 en el overworld (`--world overworld`, cuevas `mc`),
rama del worktree `agent-a0be44c1d5c42b309`, commit 7da16c2, en la 780M y en la RTX 5050. Los
conteos deterministas dan lo mismo que en f76330b.

**Bajo tierra, la cámara está en una cueva.** La primera versión medía con la cámara dentro de la
roca (`--camera 16,-1000,16,...`, vuelo y bajada): `headless_world --cull` daba `open=no` en todas
esas cámaras, y con el ojo en la roca el render ve a través de ella y el culling no oculta nada
(ADR 0022, sección 5). Esos números se sacaron. Ahora `--cave` (en `stress` y `headless_world`)
busca, cerca de la cámara pedida, el vóxel de aire con 2 vóxeles de aire alrededor, 16 o más bajo
la superficie y un camino de 64 o más vóxeles así; el escenario `path` sigue ese camino. Todas las
cámaras de cueva dan `open=yes`. El vuelo y la bajada viejos pasan 74 y 83 % de los frames con el
ojo en la roca: el resumen de `stress` ahora avisa.

**El arreglo de los chunks sin malla** (ADR 0022, sección 2): un chunk con bloques que el render no
muestra como es (sin malla todavía, o con una malla vieja) se ve a través. Con el mundo cargado no
cambia nada (los conteos de superficie de abajo dan igual). Mientras el mundo carga o se publica
una edición, oculta algo menos.

**Las corridas con ventana son válidas.** Cada una empezó sin nada compilando, sin otro binario de
boxel abierto y con `tools/wait-idle.sh 60` pasado. Un vigilante muestreó la máquina cada 0.5 s y
se tiró y repitió toda corrida en que apareció otro build o benchmark, en que el resto de la
máquina pasó de 15 % de CPU en 2 s, o en que el compositor bajó la ventana a 30 fps (pasó en 10
corridas; parece la ventana tapada). Quedaron 3 corridas por caso, intercaladas con y sin culling.

## Resultado

Criterio de la issue #19: "780M a radio 16 bajo tierra, fly y dive, con 0 frames > 6.94 ms, y BFS
≤ 0.3 ms a r16 y ≤ 1 ms a r32".

| Criterio | Objetivo | Medido | |
|---|---:|---:|---|
| 780M r16, vuelo y bajada a 64 por la roca (el criterio tal cual): frames > 6.94 ms | 0 | con culling 185 a 420 por corrida; sin, 234 a 421 | no pasa |
| 780M r16 en una cueva, quieta y girando: frames > 6.94 ms | 0 | 0 en las 6 corridas (sin culling, 34 a 211) | pasa |
| 780M r16 en una cueva, camino a 8 vóxeles/s: frames > 6.94 ms | 0 | 1, 0 y 0 (máximo 7.16 ms; sin culling, 40 a 105) | casi |
| Búsqueda a radio 16, en una cueva | 0.3 ms o menos | media 0.04 a 0.09 ms (0.13 con tope de 144), p99 0.26 (0.36) | pasa |
| Búsqueda a radio 16, superficie | 0.3 ms o menos | media 0.09 a 0.27 ms, p99 0.57 (vuelo en la 780M) | la media pasa |
| Búsqueda a radio 32, en una cueva | 1 ms o menos | media 0.16 ms (0.39 con tope), p99 0.50 (0.93), máximo 1.14 | pasa, salvo picos |
| Búsqueda a radio 32, superficie | 1 ms o menos | media 0.79 ms, p99 1.59 | la media pasa |
| Puerta del plan: caras subterráneas ocultas desde la superficie, radio 16 | 70 % o más | 15 a 45 % (27 % de media) | no pasa |
| `face_info` por chunk | 15 µs o menos | 6 a 7 µs en cuevas, 11 µs en superficie | pasa |

Lo importante:

- **El criterio de la issue no se cumple tal como está escrito.** El vuelo y la bajada atraviesan
  la roca: el ojo está en la roca 74 y 83 % de los frames, y ahí el culling se apaga para no
  ocultar lo que se ve (ADR 0022, sección 5). Con culling quedan casi igual que sin él (vuelo 378
  contra 304 fps, bajada 316 contra 294, cientos de frames lentos en las dos). Esos escenarios no
  son lo que vive un jugador.
- **Con el ojo en una cueva sí se cumple, salvo un frame.** Se dibuja de 1.8 a 4.4 veces menos (44
  a 77 % de las caras ocultas, 61 % de media en 9 cuevas). En la 780M a radio 16 pasa de 309 a 338
  fps a entre 545 y 856, y de 34 a 211 frames lentos por corrida a ninguno quieta o girando.
  Caminando por la cueva quedó 1 frame de 7.16 ms en 3 corridas.
- **A radio 32 en la RTX, casi 3 veces más fps en una cueva** (1 100 contra 436). Medido con un
  arena de 48 Mi quads: con el de 24 Mi se perdían 13 000 a 20 000 chunks, y los 1 318 y 1 401 fps
  de la versión anterior de este documento salían de eso.
- **Con tope de 144, en la 780M ayuda; en la RTX a radio 32, no.** En la 780M salta 14 a 19
  refrescos por corrida contra 66 a 79 sin culling. En la RTX salta 24 a 75 contra 7 a 18. Los
  frames tarde de la RTX no buscan más que el resto: esperan al compositor. Parece el estado de
  energía de la issue #5 (con menos trabajo, la RTX baja de estado), sin confirmar.
- **En la superficie no alcanza la puerta del plan.** Es la fuga de los chunks de 32³ que el plan
  temía: en un chunk de superficie, el aire del valle y una cueva tocan la misma cara, y el enlace
  por par de caras los une. Con cuevas cerradas (`--caves closed`) oculta 20 %: no son las
  entradas. Lo siguiente es la fase 5: enlaces por subceldas de 16³, o Hi-Z en High.
- En la superficie gana poco: 3 a 15 % más fps y un p99 algo menor, sin frames lentos nuevos.
- Las búsquedas cuestan poco en una cueva: 0.04 a 0.09 ms de media a radio 16. Volando se busca
  en 2 a 5 % de los frames (ADR 0022, sección 4); al girar o caminar mirando a otro lado, en casi
  todos. La misma búsqueda tarda más con la 780M que con la RTX (0.27 contra 0.16 ms en el mismo
  vuelo), probablemente porque la CPU comparte energía con la GPU integrada. Saliendo de la roca después de cruzar
  varios chunks, una búsqueda recarga varias capas de la grilla y llega a 2 ms.
- **Cada edición junto a la cámara apaga la ocultación unos 55 ms.** Con 20 ediciones en 20 s
  (`--edits`, quieta en la cueva), cada una de las 19 que cambian lo que ocluye apagó la ocultación
  dentro del frustum durante 51 a 60 ms (su malla nueva y `TIGHTEN_DELAY`): del todo en 9, cuando
  la edición cae en el chunk de la cámara, y en parte en 10. En total, 5 % del tiempo con menos
  ocultación y 2.5 % sin ninguna. Con 5 ediciones por segundo serían alrededor de un cuarto del
  tiempo (estimado). Ver abajo cómo se mide.

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
| Radio 32, (656, -1000, 16) | (648.5, -1014.5, 5.5) | 304, 10 | 4 895 732 | 1 522 495 | 68.9 % | 43 710 / 12 294 |

Las corridas con ventana de abajo usan la cueva cerca de (656, -1000, 16). A radio 32 los buckets
bajan de unos 43 000 a 12 000 o 14 000, lejos del tope de 65 535 draws de High.

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

En el juego (`stress`, sección "Cave culling" del resumen), por búsqueda, sumando las 3 corridas
de cada caso: la media de las medias, el peor p99 y el peor máximo. Incluye recargar las capas de
la grilla que entran al cambiar de chunk.

| Corrida | Búsquedas | Media | p99 | Máximo |
|---|---:|---:|---:|---:|
| 780M r16 en una cueva, giro | 51 352 de 51 355 frames | 0.061 ms | 0.19 ms | 0.41 ms |
| 780M r16 en una cueva, camino a 8 | 29 869 de 32 728 | 0.091 ms | 0.26 ms | 0.60 ms |
| 780M r16 en una cueva, camino a 8, tope 144 | 7 668 de 8 568 | 0.13 ms | 0.36 ms | 0.50 ms |
| 780M r16 en una cueva, quieta con `--edits` (1 corrida) | 38 de 11 864 | 0.29 ms | 0.52 ms | 0.52 ms |
| RTX r16 en una cueva, giro | 196 326 de 196 329 | 0.037 ms | 0.14 ms | 0.30 ms |
| RTX r16 en una cueva, camino a 8 | 130 605 de 143 623 | 0.051 ms | 0.16 ms | 0.39 ms |
| RTX r32 en una cueva, camino a 8 | 60 955 de 65 999 | 0.16 ms | 0.50 ms | 1.06 ms |
| RTX r32 en una cueva, camino a 8, tope 144 | 7 603 de 8 495 | 0.39 ms | 0.93 ms | 1.14 ms |
| 780M r16 superficie, vuelo 64 | 1 594 de 34 947 | 0.27 ms | 0.57 ms | 0.74 ms |
| 780M r16 superficie, giro | 35 347 de 35 350 | 0.16 ms | 0.40 ms | 0.76 ms |
| RTX r16 superficie, vuelo 64 | 2 450 de 133 240 | 0.16 ms | 0.44 ms | 0.59 ms |
| RTX r16 superficie, giro | 112 642 de 112 645 | 0.094 ms | 0.24 ms | 0.43 ms |
| RTX r32 superficie, vuelo 64 | 2 061 de 44 614 | 0.79 ms | 1.59 ms | 1.79 ms |
| 780M r16 vuelo 64 por la roca | 154 de 22 704 | 0.15 ms | 2.09 ms | 2.09 ms |
| 780M r16 bajada 64 por la roca | 120 de 18 992 | 0.42 ms | 1.96 ms | 1.96 ms |

Con tope de 144 la búsqueda tarda de 1.4 a 2.4 veces más que sin tope (probablemente la CPU, menos
ocupada, baja de frecuencia).

Los enlaces cuestan de 12 a 20 % del job de generación, que es menos de un tercio del trabajo de
un chunk (malla y luz cuestan más): alrededor de 5 % del total. Se pagan también con el culling
apagado (ADR 0022, sección 2).

## Frames con ventana

`stress`, 20 s después de 2 s de calentamiento, sin límite de fps, commit 7da16c2. En una cueva:
`--cave --camera 656,-1000,16,-90,0` (el ojo en (648.5, -1014.5, 5.5), camino de 304 vóxeles).
Superficie: `--camera 16,140,16,-90,-20`. Por la roca: `--camera 16,-1000,16,-90,0` sin `--cave`
(el vuelo y la bajada del criterio de la issue). Radio 32 con `--arena-quads 50331648
--chunk-slots 98304`. 3 corridas por caso, intercaladas con y sin culling: la media y, entre
paréntesis, la más baja y la más alta. "> 6.94 ms" da las 3 corridas.

| Caso | Culling | fps | p50 | p99 | > 6.94 ms |
|---|---|---:|---:|---:|---:|
| 780M r16 cueva, quieta | sí | 592 (579 a 600) | 1.56 ms | 3.95 (3.86 a 4.12) ms | 0, 0, 0 |
| | no | 309 (304 a 312) | 2.95 ms | 7.09 (6.93 a 7.29) ms | 211, 62, 95 |
| 780M r16 cueva, giro | sí | 856 (839 a 876) | 0.80 ms | 4.04 (3.89 a 4.23) ms | 0, 0, 0 |
| | no | 329 (324 a 334) | 2.77 ms | 6.89 (6.77 a 7.06) ms | 92, 34, 38 |
| 780M r16 cueva, camino a 8 | sí | 545 (534 a 553) | 1.59 ms | 5.34 (5.29 a 5.38) ms | 1, 0, 0 |
| | no | 338 (332 a 345) | 2.69 ms | 6.93 (6.82 a 7.09) ms | 105, 53, 40 |
| 780M r16 superficie, vuelo 64 | sí | 582 (572 a 591) | 1.58 ms | 4.18 (4.04 a 4.37) ms | 0, 0, 0 |
| | no | 507 (496 a 515) | 1.83 ms | 4.80 (4.70 a 4.91) ms | 0, 0, 0 |
| 780M r16 superficie, giro | sí | 589 (577 a 596) | 1.47 ms | 4.10 (3.98 a 4.33) ms | 0, 0, 0 |
| | no | 517 (510 a 520) | 1.80 ms | 4.64 (4.46 a 4.87) ms | 1, 0, 2 |
| 780M r16 vuelo 64 por la roca | sí | 378 (371 a 383) | 2.60 ms | 7.44 (7.23 a 7.75) ms | 420, 185, 254 |
| | no | 304 (298 a 308) | 2.99 ms | 7.55 (7.34 a 7.81) ms | 421, 234, 284 |
| 780M r16 bajada 64 por la roca | sí | 316 (312 a 320) | 2.97 ms | 7.55 (7.45 a 7.66) ms | 390, 307, 315 |
| | no | 294 (291 a 296) | 3.12 ms | 7.53 (7.47 a 7.63) ms | 364, 333, 353 |
| 780M r16 cueva, quieta con `--edits` (1 corrida) | sí | 593 | 1.54 ms | 3.96 ms | 2 |
| | no | 315 | 2.92 ms | 6.93 ms | 58 |
| RTX r16 cueva, quieta | sí | 2 600 (2 596 a 2 603) | 0.35 ms | 0.85 (0.84 a 0.86) ms | 0, 0, 0 |
| | no | 1 489 (1 478 a 1 496) | 0.61 ms | 1.83 (1.81 a 1.85) ms | 0, 1, 0 |
| RTX r16 cueva, giro | sí | 3 272 (3 197 a 3 335) | 0.29 ms | 0.81 (0.79 a 0.84) ms | 0, 0, 0 |
| | no | 1 513 (1 510 a 1 518) | 0.60 ms | 1.80 (1.78 a 1.81) ms | 0, 0, 0 |
| RTX r16 cueva, camino a 8 | sí | 2 394 (2 382 a 2 401) | 0.37 ms | 1.06 (1.05 a 1.09) ms | 0, 0, 0 |
| | no | 1 578 (1 571 a 1 583) | 0.58 ms | 1.78 (1.78 a 1.79) ms | 0, 0, 0 |
| RTX r16 superficie, vuelo 64 | sí | 2 221 (2 211 a 2 227) | 0.40 ms | 1.03 (1.00 a 1.05) ms | 0, 0, 0 |
| | no | 1 992 (1 985 a 1 997) | 0.46 ms | 1.10 (1.09 a 1.11) ms | 0, 0, 0 |
| RTX r16 superficie, giro | sí | 1 877 (1 822 a 1 951) | 0.49 ms | 1.33 (1.07 a 1.50) ms | 0, 0, 0 |
| | no | 1 826 (1 819 a 1 836) | 0.50 ms | 1.55 (1.53 a 1.58) ms | 0, 0, 0 |
| RTX r32 cueva, quieta | sí | 1 104 (1 079 a 1 121) | 0.82 ms | 2.26 (2.20 a 2.36) ms | 0, 0, 0 |
| | no | 393 (387 a 396) | 2.41 ms | 5.66 (5.58 a 5.77) ms | 6, 2, 0 |
| RTX r32 cueva, camino a 8 | sí | 1 100 (1 082 a 1 130) | 0.76 ms | 3.44 (3.40 a 3.46) ms | 0, 0, 0 |
| | no | 436 (435 a 437) | 2.19 ms | 5.73 (5.71 a 5.75) ms | 1, 1, 0 |
| RTX r32 superficie, vuelo 64 | sí | 744 (740 a 746) | 1.20 ms | 3.34 (3.32 a 3.37) ms | 0, 0, 0 |
| | no | 670 (666 a 673) | 1.32 ms | 3.72 (3.70 a 3.73) ms | 0, 1, 0 |

- En una cueva la diferencia es grande y se repite: de 1.6 a 2.6 veces los fps en la 780M y de
  1.5 a 2.8 en la RTX, y ningún frame lento quieta o girando. El único que queda caminando (7.16
  ms) no hace trabajo de mundo; la búsqueda (0.14 ms) y los bits del render (1.4 ms) suman 1.6 ms
  de ese frame.
- Con el ojo en una cueva, ningún frame ve a través de la roca (`blocked` en 0). Por la roca, 74 %
  de los frames del vuelo y 83 % de la bajada no ocultan nada, y el resumen avisa.
- En la superficie gana poco y siempre: 3 a 15 % más fps.
- Ningún chunk perdido en ninguna corrida. A radio 32 bajo tierra hace falta el arena de 48 Mi
  quads: con el de 24 Mi (el del overworld) se pierden 13 000 a 20 000 chunks, y los fps suben
  porque falta dibujar (los 1 318 y 1 401 fps de la versión anterior de este documento).

### Con tope de 144

`--fps-limit 144`, el ritmo de un juego, caminando por la cueva a 8 vóxeles/s. Refrescos saltados:
intervalos entre frames presentados de más de 1.5 veces la mediana.

| Caso | Culling | Presentados | Refrescos saltados | p99 del frame |
|---|---|---:|---:|---:|
| 780M r16 | sí | 2 853 a 2 858 | 14, 18, 19 | 7.65 ms |
| | no | 2 792 a 2 807 | 66, 79, 71 | 13.89 ms |
| RTX r32 | sí | 2 798 a 2 849 | 75, 38, 24 | 11.84 (7.72 a 14.03) ms |
| | no | 2 853 a 2 865 | 18, 17, 7 | 7.63 ms |

- En la 780M el culling quita casi todos los refrescos saltados.
- En la RTX a radio 32 los aumenta. Los frames tarde no buscan más ni rearman más bits que el
  resto (0.3 a 0.5 ms cada cosa, como la media): esperan más a la compuerta de presentación. Lo
  más probable es la issue #5: con menos trabajo por frame, la RTX baja de estado de energía y a
  veces termina tarde. Sin confirmar.

## Costo de las ediciones

El promedio de chunks ocultos del resumen de `stress` no muestra este costo: cuenta también los
chunks que el frustum deja fuera, que desde esta cueva son la mayoría. Con ediciones baja de
11 604 a 11 560, menos de 1 %, aunque la ocultación de cuevas se apague del todo un rato.

Lo que sirve es la columna `occluded` del CSV, frame a frame, con la cámara quieta. Sin ediciones
vale siempre 11 604. Con 20 ediciones en 20 s (la corrida `--edits` de arriba) baja en 19 tramos
de 51 a 60 ms, uno por edición que cambia lo que ocluye, y vuelve a 11 604:

| Estado | Tramos | Chunks ocultos | Ocultos por la cueva dentro del frustum | Tiempo |
|---|---:|---:|---:|---:|
| Sin edición pendiente | - | 11 604 | 1 963 de 3 339 (59 %) | 94.8 % |
| Edición que apaga una parte | 10 | 10 856 | 1 215 (62 % de lo normal) | 2.7 % |
| Edición que apaga todo | 9 | 9 641 | ninguno | 2.5 % |

En los tramos que apagan todo, la búsqueda ve a través del chunk de la cámara y el conjunto deja
pasar todo lo que está en el frustum: esos 9 641 son lo que ocultan solos el frustum y la caja de
búsqueda. La ocultación de cuevas dentro del frustum es el resto. El tiempo cuenta la duración de
los frames, que en esos tramos son más lentos.

## Cómo volver a medir (para el dueño)

Todo en release, con la máquina quieta, intercalando con y sin culling, 3 vueltas o más. **Bajo
tierra, siempre con `--cave`:** sin él la cámara puede estar en la roca, donde no se oculta nada
(`stress` lo avisa en el resumen). `fly` y `dive` atraviesan la roca: `stress` no los acepta con
`--cave`; para moverse por una cueva está `path`. **A radio 32 bajo tierra, con
`--arena-quads 50331648 --chunk-slots 98304`:** con el arena por defecto se pierden chunks (el
resumen da `chunks dropped`). Tirar toda corrida con algún segundo a 30 fps en "Per second": el
compositor frenó la ventana (tapada u oculta).

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
$HW --world overworld --view-distance 32 --hz 0 --cull --cave --camera 656,-1000,16,-90,0 --timeout 300

# Benches de la búsqueda y de los enlaces
tools/wait-idle.sh 60 && cargo bench -p boxel_world --bench cave_culling

# Frames con ventana (cada receta espera a que la CPU se calme y marca -busy si no)
CAVE="--world overworld --cave --camera 656,-1000,16,-90,0"
SURF="--world overworld --camera 16,140,16,-90,-20"
ROCK="--world overworld --camera 16,-1000,16,-90,0"
BIG="--arena-quads 50331648 --chunk-slots 98304"
for round in 1 2 3; do
  for c in on off; do
    for gpu in 780m nvidia; do
      just bench-static $CAVE --adapter $gpu --cave-culling $c
      just bench-spin   $CAVE --adapter $gpu --cave-culling $c
      just bench-path   $CAVE --speed 8 --adapter $gpu --cave-culling $c
      just bench-fly    $SURF --speed 64 --adapter $gpu --cave-culling $c
      just bench-spin   $SURF --adapter $gpu --cave-culling $c
    done
    just bench-fly  $ROCK --speed 64 --adapter 780m --cave-culling $c   # el criterio de la issue
    just bench-dive $ROCK --speed 64 --adapter 780m --cave-culling $c
    just bench-static $CAVE --radius 32 $BIG --adapter nvidia --cave-culling $c
    just bench-path   $CAVE --speed 8 --radius 32 $BIG --adapter nvidia --cave-culling $c
    just bench-fly    $SURF --speed 64 --radius 32 $BIG --adapter nvidia --cave-culling $c
    # Con el límite del juego: refrescos saltados (Pacing, "over 1.5 x p50")
    just bench-path $CAVE --speed 8 --fps-limit 144 --adapter 780m --cave-culling $c
    just bench-path $CAVE --speed 8 --radius 32 $BIG --fps-limit 144 --adapter nvidia --cave-culling $c
  done
done
just bench-static $CAVE --edits --adapter 780m --cave-culling on   # costo: columna occluded del CSV
just bench-report --compare <corrida-off>.md <corrida-on>.md
```

`tools/bench_report.py` empareja una corrida con culling con las de main de antes (no separa por
culling) y anota la diferencia; compara además los chunks ocultos, los draws y el tiempo de
búsqueda. Las corridas con `--cave` solo se emparejan entre ellas (el título termina en `cave`).
