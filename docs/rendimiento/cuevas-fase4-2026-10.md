# Cuevas, fase 4: ocultación de cuevas (2026-10-08)

Qué se midió: el cave culling del ADR 0022 en el overworld (`--world overworld`, cuevas `mc`),
rama del worktree `agent-a0be44c1d5c42b309`, commit 9e3e682 para las corridas con tiempo.

**Corrección posterior:** las corridas con ventana bajo tierra (vuelo y bajada) llevan la cámara
en línea recta por dentro de la roca. Ahí el render ve a través de la roca, y el culling ocultaba
cosas que sí estaban en pantalla. Ahora no se oculta nada mientras el ojo está en la roca (ADR
0022, sección 5), así que los números con ventana bajo tierra de abajo ya no valen. Los conteos de
`headless_world` sí: valen para un ojo en espacio abierto del mismo chunk, y la línea `cull:`
dice ahora `open=no` cuando la cámara está en la roca (así salen las cuatro cámaras bajo tierra de
la tabla; la de superficie sale `open=yes`). El resumen de `stress` cuenta los frames sin culling
por eso. Falta medir con ventana bajo tierra con la cámara en una cueva, y con el límite de 144
fps (ver el final).

**La máquina estaba compartida con otro trabajo largo** (carga de 3.5 a 8). `tools/wait-idle.sh`
dejó pasar todas las corridas (menos de 15 % de CPU ocupada en el momento de empezar), pero los
tiempos son orientativos: el dueño los vuelve a medir con los comandos del final. Los conteos de
draws y caras no dependen de la carga.

## Resultado

| Criterio | Objetivo | Medido | |
|---|---:|---:|---|
| Puerta del plan: caras subterráneas ocultas desde la superficie, radio 16 | 70 % o más | 15 a 45 % (27 % de media) | no pasa |
| 780M radio 16 bajo tierra, vuelo 64: frames > 6.94 ms | 0 | 0 y 0 | pasa |
| 780M radio 16 bajo tierra, bajada 64: frames > 6.94 ms | 0 | 4 y 6 (máximo 8.2 ms) | casi |
| Búsqueda a radio 16 | 0.3 ms o menos | bajo tierra 0.12 a 0.21 ms; superficie 0.37 ms por búsqueda | bajo tierra sí |
| Búsqueda a radio 32 | 1 ms o menos | bajo tierra 0.2 ms; superficie 0.8 ms de media, p99 1.3 ms | casi |
| `face_info` por chunk | 15 µs o menos | 6 a 7 µs en cuevas, 11 µs en superficie | pasa |

Lo importante:

- **Bajo tierra funciona:** se dibuja entre 1.6 y 25 veces menos (62 % de las caras ocultas de
  media en vuelo, 67 % en bajada). En la 780M el vuelo pasa de unos 270 a 2 300 fps y queda sin
  frames lentos. La bajada deja 4 a 6 frames apenas por encima de 6.94 ms, todos de GPU, en
  cavernas grandes que sí se ven.
- **En la superficie no alcanza la puerta del plan.** Es la fuga de los chunks de 32³ que el plan
  temía: en un chunk de superficie, el aire del valle y una cueva tocan la misma cara, y el enlace
  por par de caras los une. Con cuevas cerradas (`--caves closed`) oculta 20 %: no son las
  entradas. Lo siguiente es la fase 5: enlaces por subceldas de 16³, o Hi-Z en High.
- Las búsquedas son pocas: con la reutilización (ADR 0022, sección 4), volando se busca en 1 a
  4 % de los frames. Al girar en el lugar se busca en cada frame (0.15 ms de media en la
  superficie).

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
| Bajo tierra (16, -1000, 16), horizontal | 1 085 900 | 267 161 | 75.4 % | 9 576 / 2 160 | 75.4 % |
| Bajo tierra, x = 336 | 1 013 710 | 363 411 | 64.2 % | 9 446 / 3 183 | 64.2 % |
| Bajo tierra, x = 656 | 1 061 598 | 473 770 | 55.4 % | 9 510 / 3 744 | 55.4 % |
| Bajo tierra, x = 976 | 1 055 947 | 560 264 | 46.9 % | 9 462 / 4 624 | 46.9 % |
| Bajo tierra, x = 1296 | 1 065 584 | 341 923 | 67.9 % | 9 411 / 2 729 | 67.9 % |
| Bajada, y = -1320 | 1 137 429 | 663 099 | 41.7 % | 9 609 / 5 162 | 41.7 % |
| Bajada, y = -1640 | 1 146 755 | 707 652 | 38.3 % | 9 612 / 5 411 | 38.3 % |
| Bajada, y = -1960 | 1 056 222 | 166 719 | 84.2 % | 9 516 / 1 469 | 84.2 % |
| Bajada, y = -2280 | 1 079 081 | 43 758 | 95.9 % | 9 532 / 434 | 95.9 % |
| `forest`, superficie | 263 115 | 263 110 | 0.0 % | 1 490 / 1 489 | - |
| Radio 32, superficie | 2 731 252 | 2 471 860 | 9.5 % | 20 200 / 16 819 | 15.8 % |
| Radio 32, bajo tierra | 4 863 680 | 922 678 | 81.0 % | 43 569 / 7 507 | 81.0 % |

Las posiciones de vuelo y bajada son las que pasa `stress` (vuelo a lo largo de +x, bajada en y).
A radio 32 bajo tierra los buckets bajan de 43 569 a 7 507, lejos del tope de 65 535 draws de
High.

Sin frustum en la búsqueda (para buscar solo al cambiar de chunk) se oculta bastante menos: 57.6
en vez de 75.4 % a y = -1000, 63.5 en vez de 95.9 % a -2280, 69.7 en vez de 81.0 % a radio 32.
Por eso la búsqueda usa el frustum.

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

En el juego (`stress`, sección "Cave culling" del resumen), por búsqueda:

| Corrida | Búsquedas | Media | p99 | Máximo |
|---|---:|---:|---:|---:|
| 780M r16 bajo tierra, vuelo 64 | 429 de 46 573 frames | 0.12 ms | 0.39 ms | 0.48 ms |
| 780M r16 bajada 64 | 279 de 29 489 | 0.20 ms | 0.50 ms | 0.53 ms |
| 780M r16 superficie, vuelo 64 | 385 de 9 470 | 0.37 ms | 0.71 ms | 0.85 ms |
| 780M r16 superficie, giro | 9 894 de 9 895 | 0.15 ms | 0.34 ms | 0.77 ms |
| RTX r32 bajo tierra, vuelo 64 | 458 de 71 323 | 0.20 ms | 0.64 ms | 0.90 ms |
| RTX r32 superficie, vuelo 64 | 384 de 12 920 | 0.81 ms | 1.33 ms | 1.94 ms |

Los enlaces cuestan de 12 a 20 % del job de generación, que es menos de un tercio del trabajo de
un chunk (malla y luz cuestan más): alrededor de 5 % del total. Se pagan también con el culling
apagado (ADR 0022, sección 2).

## Frames con ventana

`stress`, 20 s, sin límite de fps, 2 corridas por caso (la segunda vuelta entre paréntesis).
Cámaras: bajo tierra `16,-1000,16,-90,0`, superficie `16,140,16,-90,-20`.

| Caso | Culling | fps | p99 | > 6.94 ms |
|---|---|---:|---:|---:|
| 780M r16 bajo tierra, vuelo 64 | sí | 2 329 (2 230) | 3.39 (3.45) ms | 0 (0) |
| | no | 274 (270) | 9.27 (9.31) ms | 763 (842) |
| 780M r16 bajada 64 | sí | 1 474 (1 440) | 4.95 (4.90) ms | 4 (6) |
| | no | 269 (266) | 8.76 (8.59) ms | 862 (921) |
| 780M r16 superficie, vuelo 64 | sí | 473 (543) | 6.28 (4.79) ms | 42 (7) |
| | no | 480 (484) | 5.25 (5.23) ms | 2 (0) |
| 780M r16 superficie, giro | sí | 495 | 5.64 ms | 10 |
| | no | 476 | 5.39 ms | 4 |
| RTX r32 bajo tierra, vuelo 64 | sí | 3 566 | 1.92 ms | 0 |
| | no | 575 | 6.13 ms | 4 |
| RTX r32 superficie, vuelo 64 | sí | 646 | 4.31 ms | 3 |
| | no | 593 | 4.34 ms | 8 |

- Bajo tierra la diferencia es enorme y se repite. En la superficie las dos versiones quedan
  dentro del ruido de la máquina ocupada (la primera vuelta con culling tuvo un frame de 29.7 ms
  sin trabajo del motor).
- Los frames lentos que quedan con culling no hacen trabajo de mundo ni buscan: esperan a la GPU.
- RTX r32 bajo tierra pierde 26 600 chunks por el arena lleno (fase 5), con y sin culling.
- Una corrida anterior de la RTX r32 tuvo frames de 33 ms en los primeros 2.4 s sin trabajo y sin
  presentar; repetida no apareció. Parece del entorno (la ventana), no del culling.

## Cómo volver a medir (para el dueño)

Todo en release, con la máquina quieta, intercalando con y sin culling, 3 vueltas o más.

```bash
# Conteos deterministas (no hace falta máquina quieta)
cargo build --release --locked --example headless_world
HW=target/release/examples/headless_world
for c in 16,140,16,-90,-20 336,140,16,-90,-20 656,140,16,-90,-20 16,-1000,16,-90,0 \
         656,-1000,16,-90,0 16,-1640,16,-90,0 16,-2280,16,-90,0; do
  $HW --world overworld --view-distance 16 --hz 0 --cull --camera $c
done
$HW --world overworld --view-distance 32 --hz 0 --cull --camera 16,140,16,-90,-20 --timeout 300
$HW --world overworld --view-distance 32 --hz 0 --cull --camera 16,-1000,16,-90,0 --timeout 300

# Benches de la búsqueda y de los enlaces
tools/wait-idle.sh 60 && cargo bench -p boxel_world --bench cave_culling

# Frames con ventana (cada receta espera a que la CPU se calme y marca -busy si no)
for round in 1 2 3; do
  for c in on off; do
    just bench-fly  --world overworld --speed 64 --camera 16,-1000,16,-90,0 --adapter 780m --cave-culling $c
    just bench-dive --world overworld --speed 64 --camera 16,-1000,16,-90,0 --adapter 780m --cave-culling $c
    just bench-fly  --world overworld --speed 64 --camera 16,140,16,-90,-20 --adapter 780m --cave-culling $c
    just bench-spin --world overworld --camera 16,140,16,-90,-20 --adapter 780m --cave-culling $c
    just bench-fly  --world overworld --speed 64 --radius 32 --camera 16,-1000,16,-90,0 --adapter nvidia --cave-culling $c
    just bench-fly  --world overworld --speed 64 --radius 32 --camera 16,140,16,-90,-20 --adapter nvidia --cave-culling $c
  done
done
# Bajo tierra con la cámara en una cueva: elegir una posición con `open=yes` en
# `headless_world --cull --camera ...` (las de vuelo y bajada atraviesan la roca y no ocultan nada)
just bench-spin --world overworld --camera <x,y,z de una cueva>,-90,0 --adapter 780m --cave-culling $c
# Con el límite del juego: ¿la búsqueda hace perder refrescos? (presupuesto del mundo, ADR 0022)
just bench-fly  --world overworld --speed 64 --radius 32 --camera 16,140,16,-90,-20 --adapter nvidia --fps-limit 144
just bench-dive --world overworld --speed 64 --radius 32 --camera 16,140,16,-90,-20 --adapter nvidia --fps-limit 144
just bench-report --compare <corrida-off>.md <corrida-on>.md
```

`tools/bench_report.py` empareja una corrida con culling con las de main de antes (no separa por
culling) y anota la diferencia; compara además los chunks ocultos, los draws y el tiempo de
búsqueda.
