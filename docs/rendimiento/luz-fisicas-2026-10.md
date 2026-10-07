# Luz y física: lo que cuestan, P7 (2026-10-06)

Resumen de las dos mediciones de P7. Todas las tablas están en
[luz-p7-2026-10](luz-p7-2026-10.md) y [fisicas-p7-2026-10](fisicas-p7-2026-10.md).

## En corto

- La luz cumple en la RTX a radio 16, salvo las ediciones sin tope de fps.
- En la 780M cumple en el bosque. En la superficie del overworld no: p99 de 4.20 ms (objetivo 4)
  y un frame de 8.0 ms. Ahí el límite es la GPU.
- La física cumple la regla del frame con 1000 y 10 000 objetos dormidos, 1000 mobs y 1000
  flechas. Con 10 000 mobs despiertos no.

## Qué se midió y cómo

- **Luz** ([ADR 0015](../adr/0015-luz-de-voxeles.md), fase P7). Tres versiones:
  - **antes**: 35d1eb0 (física P2), sin luz.
  - **luz**: master e895a11, como se entrega (`--light on`).
  - **sin luz**: el mismo binario con `--light off`, sin luz ni AO.

  "Sin luz" separa lo que cuesta prender la luz de lo demás que cambió desde antes.
- **Física** ([ADR 0016](../adr/0016-fisicas-propias.md), regla del frame, 4.9): bosque, radio
  16, cámara quieta, con luz. Sin cuerpos contra 1000 y 10 000 objetos, 1000 y 10 000 mobs y 1000
  flechas.
- `examples/stress.rs` en release, 20 s medidos tras 2 s de warmup, ventana 1280x720, radio
  vertical 8. RTX 5050 (Vulkan High, driver 595.91.07) y 780M (RADV, Mesa 26.0.8, Vulkan High).
  Cada resumen dice en qué GPU corrió.
- Binarios comprobados por md5 y con `strings`: solo los nuevos tienen `--light` y solo los de
  trace tienen los spans.
- 3 corridas por configuración, intercaladas y en orden rotado. `tools/wait-idle.sh 60` antes de
  cada una, nada compilando. 174 corridas de luz y 48 de física: ninguna salió ocupada ni se
  cruzó con otra.
- Significancia: test de permutación exacto sobre la media de cada corrida, más los rangos. Con 3
  corridas contra 3 el p más chico posible es 0.1. "Se separan" quiere decir p = 0.1 y rangos que
  no se tocan.
- Frame de cruce: el frame en que la cámara pasa a otro chunk. Puesta al día: desde ese frame
  hasta que el mundo terminó los chunks nuevos.
- Los datos crudos están en `bench-results/p7-2026-10-06/` (no está en git).

## Luz por escenario

Cada celda: antes / luz / sin luz. Media de 3 corridas. Bajo tierra es el overworld con la cámara
en y = -1000.

### RTX 5050, radio 16, sin tope

| Escenario | p50 ms | p99 ms | Frame de cruce ms | Puesta al día ms | Quads |
|---|---|---|---|---|---|
| Bosque, vuelo 32 | 0.103 / 0.181 / 0.153 | 0.52 / 0.62 / 0.56 | 1.28 / 1.49 / 1.37 | 5.2 / 8.1 / 5.4 | 0.51 / 0.89 / 0.51 M |
| Overworld superficie, vuelo 64 | 0.316 / 0.397 / 0.361 | 0.80 / 0.97 / 0.89 | 1.57 / 1.91 / 1.63 | 9.5 / 12.9 / 9.7 | 3.60 / 3.98 / 3.60 M |
| Bajo tierra, vuelo 64 | 0.517 / 0.555 / 0.558 | 1.62 / 1.73 / 1.74 | 1.91 / 2.07 / 1.94 | 20.2 / 21.4 / 19.4 | 9.65 M las tres |
| Bajo tierra, bajada 64 | 0.543 / 0.581 / 0.585 | 1.68 / 1.79 / 1.78 | 1.79 / 1.98 / 1.57 | 26.1 / 25.6 / 25.4 | 9.90 M las tres |

- Con luz ningún frame pasó de 6.94 ms en las 12 corridas. El peor frame de cruce fue de 5.51 ms
  (superficie).
- En el bosque y la superficie la luz se separa de antes y de sin luz en p50, p99, frame de cruce
  y puesta al día. El AO parte las caras: 75 % más quads en el bosque y 11 % en la superficie.
- Bajo tierra casi no se nota. Luz y sin luz dan el mismo frame, y lo poco que sube contra antes
  ya está sin luz. Las caras a oscuras no llevan AO, así que los quads no cambian.
- El trabajo extra al cruzar es sobre todo recoger los jobs de malla (`world::run_jobs`): 2.6 ms
  con luz contra 1.2 ms en la superficie.

### RTX 5050, radio 32, sin tope

| Escenario | p99 ms | Frame de cruce ms | Puesta al día ms | Arena usado, pico |
|---|---|---|---|---|
| Bosque, vuelo 64 | 0.73 / 1.01 / 0.80 | 1.44 / 1.89 / 1.45 | 9.4 / 15.0 / 9.7 | 22.8 / 41.2 / 22.9 % |
| Overworld superficie, vuelo 64 | 2.62 / 3.15 / 2.74 | 2.08 / 2.74 / 1.99 | 19.6 / 25.8 / 19.0 | 56.5 / 63.0 / 56.6 % |

- Los objetivos del plan son a radio 16. A radio 32 en la superficie, el frame de cruce y la
  puesta al día con luz quedan apenas arriba de esos números.
- Hubo frames de más de 6.94 ms (7.0 y 7.4 ms con luz, 10.0 ms sin luz) sin trabajo del mundo:
  son los picos conocidos de la NVIDIA.

### 780M, radio 16

| Escenario | p50 ms | p99 ms | Frames > 6.94 ms en 3 corridas | Frame de cruce ms | Puesta al día ms |
|---|---|---|---|---|---|
| Bosque, vuelo 32 | 0.583 / 0.732 / 0.608 | 1.71 / 2.03 / 1.73 | 0 / 0 / 0 | 1.69 / 2.04 / 1.66 | 7.2 / 10.9 / 7.4 |
| Overworld superficie, vuelo 64 | 1.404 / 1.620 / 1.490 | 3.72 / 4.20 / 3.90 | 0 / 1 / 0 | 2.44 / 3.04 / 2.62 | 13.3 / 17.6 / 13.4 |

- En la superficie del overworld la 780M va limitada por la GPU: con luz espera a la GPU 1.32 ms
  de unos 1.7 ms por frame (antes 1.13). Se muestran menos frames: 119 por segundo contra 137
  antes.
- El frame de 8.0 ms fue un frame mostrado que subió 42 mallas.

Memoria: ningún chunk perdido en ninguna corrida. El arena llega como mucho al 63 % (radio 32,
superficie, con luz; antes 57 %).

## Costo de GPU de la luz

Cámara quieta, radio 16, p50 del frame en ms. Antes / luz / sin luz.

| GPU | Bosque | Overworld superficie | Objetivo | |
|---|---|---|---|---|
| RTX 5050 | 0.107 / 0.195 / 0.163 | 0.317 / 0.408 / 0.367 | overworld 0.45 ms o menos | pasa |
| 780M | 0.542 / 0.701 / 0.580 | 1.259 / 1.484 / 1.371 | 0.3 ms o menos sobre antes | pasa (+0.16 y +0.23) |

- Prender la luz (luz contra sin luz) cuesta 0.03 a 0.04 ms en la RTX y 0.11 a 0.12 ms en la
  780M. Todo se separa.
- Sin luz no es el mundo de antes: con los mismos quads, en la RTX ya cuesta unos 0.05 ms más. El
  render nuevo cuesta algo aunque la luz esté apagada.

## Con tope de 144 fps

RTX, overworld radio 16, vuelo 64. Antes / luz.

| Medida | Superficie | Bajo tierra |
|---|---|---|
| Frames > 7.5 ms por corrida | 97 (74 a 122) / 61 (6 a 127) | 94 (80 a 121) / 73 (49 a 120) |
| Frames > 10.4 ms | 0 / 0 | 0 / 0 |
| Trabajo por frame, p99 ms | 2.78 / 3.05 | 3.30 / 3.46 |
| Puesta al día ms | 17.1 / 24.8 | 27.3 / 34.9 |

- Los frames de más de 7.5 ms no suben (p = 0.5): pasa. Son la espera al permiso de Wayland para
  mostrar, no trabajo.
- La puesta al día sí sube, unos 8 ms en los dos casos (p = 0.1). Bajo tierra ya pasaba de 25 ms
  antes. El plan no pone objetivo de puesta al día con tope.

## Latencia de ediciones

RTX, overworld radio 16, cámara quieta, `--edits`: 20 ediciones medidas por corrida (lámpara
puesta y sacada, pozo cavado y tapado).

| Medida | Luz, sin tope | Sin luz, sin tope | Luz, tope 144 |
|---|---|---|---|
| Frames hasta publicar, mediana / peor | 4 / 5 a 6 | 2 / 3 | 2 / 2 |
| Tiempo hasta publicar, mediana / peor ms | 1.85 / 3.37 | 0.84 / 2.36 | 13.9 / 14.6 |
| Lotes partidos por corrida (de 20) | 1 a 3 | 0 | 0 |

- Con luz cada edición remalla 8.6 chunks más (172 por corrida), por la luz que cambia.
- Sin tope un frame dura menos de medio ms, así que esperar 2 frames por el lote
  (`edit_batch_hold_frames`) es menos de 1 ms, menos que un job de malla con luz. Por eso se
  parten lotes. Con tope los 2 frames son 14 ms y cumple.

## Física a escala

RTX, bosque radio 16, cámara quieta, con luz. La regla: ningún frame nuevo de más de 6.94 ms y el
p99 sube 0.3 ms como mucho, contra la corrida sin cuerpos.

| Configuración | Frames > 6.94 ms en 3 corridas | p99 ms | Sube el p99 | Paso medio ms | Con tope: frames > 7.5 ms por corrida | Regla |
|---|---|---|---|---|---|---|
| Sin cuerpos | 0 | 0.646 | | 0.013 | 54 | |
| 1000 objetos | 0 | 0.640 | -0.006 | 0.081 | 12 | cumple |
| 10 000 objetos | 0 | 0.657 | +0.011 | 0.203 | 83 | cumple |
| 1000 mobs | 0 | 0.735 | +0.088 | 0.580 | 45 | cumple |
| 10 000 mobs | 5 | 4.263 | +3.62 | 3.77 | 954 | no cumple |
| 1000 flechas | 0 | 0.655 | +0.009 | 0.277 | 67 | cumple |

El paso medio es por frame con paso, de una corrida con `--spans` sin tope.

- Con 10 000 mobs despiertos el paso cuesta 3.8 ms (p99 5.1, el peor 8.9), casi el doble que en
  el bench (2.06 ms): movimiento 1.14, broadphase 0.96 y solve 1.19 ms. Con tope se muestran 96
  frames por segundo en vez de 141.
- Los objetos están todos dormidos al medir. Se deduce del costo de los spans, porque `stress` no
  imprime `PhysicsStats`. Los mobs no se duermen nunca.
- Con tope, los frames de 8.7 ms que salen también sin cuerpos son el runner esperando al
  compositor, no la física.
- Ningún frame corrió dos pasos.

## Qué pasa la aceptación y qué no

| Qué | Resultado |
|---|---|
| Luz, RTX radio 16 sin tope: frames, p99, cruce, puesta al día, GPU | pasa |
| Luz, RTX con tope de 144: frames > 7.5 ms | pasa |
| Luz, ediciones sin tope | no pasa: 1 a 3 lotes partidos de 20 por corrida |
| Luz, ediciones con tope de 144 | pasa |
| Luz, 780M bosque | pasa |
| Luz, 780M overworld superficie | no pasa: p99 4.20 ms y un frame de 8.0 ms |
| Luz, tiempo extra de GPU en la 780M | pasa |
| Memoria: chunks perdidos y arena | pasa |
| Física: 1000 y 10 000 objetos, 1000 mobs, 1000 flechas | pasa |
| Física: 10 000 mobs despiertos | no pasa |
| Física: el caso del plan (9000 dormidos y 1000 despiertos), radio 32, vuelo, bajada, 780M | sin medir |

La referencia de hoy da un poco distinto a los valores del plan: puesta al día bajo tierra 20.2
ms (el plan dice 18.7) y bajada 26.1 ms (el plan dice 25.0).

## Qué hacer ahora

1. Ediciones: esperar el lote por tiempo y no por frames. Probablemente arregla el caso sin tope;
   no se probó.
2. 780M en la superficie: decidir entre culling de cuevas y otra política de AO (sacar el AO de la
   clave del merge). Antes conviene medir el AO aparte de la luz, porque hoy `--light off` apaga
   los dos.
3. Física: bajar el paso con 10 000 despiertos (broadphase y solve siguen casi todo en un hilo).
   Que `stress` mezcle tipos de cuerpo e imprima `PhysicsStats`, y medir el caso del plan en las
   dos GPU, a radio 16 y 32, quieto, volando y bajando.
4. Revisar d8c67a0 antes de mergear `measure/p7`: agrega un campo público en `boxel_render`
   (`RenderInfo::arena_used_quads`).
5. Enlazar esta medición en los ADR 0015 (todavía dice P7 pendiente) y 0016, con lo que decida el
   dueño.
