# Física a escala: la regla del frame, P7 (2026-10-06)

Qué se midió: lo que agregan los cuerpos de física al frame del juego, con `examples/stress.rs` en
la RTX 5050. Es la regla del frame del plan de física (sección 4.9,
[ADR 0016](../adr/0016-fisicas-propias.md)): con 1000 y con 10 000 cuerpos, ningún frame nuevo de
más de 6.94 ms y el p99 sube 0.3 ms como mucho, contra la misma corrida sin cuerpos.

Caso: bosque, radio 16, cámara quieta, con luz (master). Cinco configuraciones contra la
referencia sin cuerpos:

- **1000 objetos** y **10 000 objetos** (`--body-kind item`): caen, se apoyan y se duermen.
- **1000 mobs** y **10 000 mobs** (`--body-kind mob`): caminan y saltan, nunca se duermen.
- **1000 flechas** (`--projectiles 1000`): cada una se tira de nuevo cada 2 s, la mitad está en
  el aire.

## Resultado

Sin tope de fps. Media de 3 corridas; entre paréntesis, la más baja y la más alta.

| Configuración | Frames > 6.94 ms | p99 ms | Sube el p99 | Peor frame ms | Regla |
|---|---|---|---|---|---|
| Sin cuerpos | 0 | 0.646 (0.640 a 0.651) | | 4.19 (4.17 a 4.23) | |
| 1000 objetos | 0 | 0.640 | -0.006 | 4.86 (4.26 a 5.74) | cumple |
| 10 000 objetos | 0 | 0.657 | +0.011 | 4.13 | cumple |
| 1000 mobs | 0 | 0.735 (0.716 a 0.758) | +0.088 | 4.56 | cumple |
| 10 000 mobs | 5 en 3 corridas | 4.263 (4.224 a 4.335) | +3.62 | 8.82 (7.80 a 9.88) | no cumple |
| 1000 flechas | 0 | 0.655 | +0.009 | 4.08 | cumple |

Con tope de 144 fps (3 corridas por configuración) sale lo mismo: solo se notan los 10 000 mobs.
Con ellos el trabajo de 16 frames pasa de 6.94 ms y se muestran 96 frames por segundo (91 a 101)
en vez de 141.

### Lo importante

- La regla se cumple con 1000 y 10 000 objetos dormidos, con 1000 mobs despiertos y con 1000
  flechas. El p99 sube 0.09 ms como mucho (1000 mobs, p = 0.1) y no aparece ningún frame lento.
- 10 000 mobs despiertos no la cumplen: un paso cuesta 3.8 ms de media (p99 5.1, el peor 8.9).
  Sin tope cae en uno de cada 50 frames, más o menos; con tope de 144, en uno de cada 2.25. Es
  más que el caso del plan (10 000 con 90 % dormidos), pero muestra el techo: 10 000 despiertos no
  caben en la regla.
- Dentro del juego ese paso cuesta casi el doble que en el bench: 3.8 ms contra 2.06 ms de
  `scale/10000_awake`. Movimiento 1.14, broadphase 0.96 y solve 1.19 ms. No es el costo de los
  spans: sin trace el p99 del frame da casi lo mismo (4.26 contra 4.34 ms).
- El caso exacto del plan (9000 dormidos y 1000 despiertos juntos) no se midió, porque `stress`
  tiene un solo tipo de cuerpo por corrida. Por las partes (10 000 objetos dormidos cuestan 0.20
  ms por paso y 1000 mobs 0.58 ms) debería cumplir, pero falta medirlo.
- Con tope, los frames de 8.7 ms no son de la física: son el runner esperando el permiso del
  compositor para mostrar (hasta un cuarto del período, 1.74 ms), y el frame siguiente dura 5.2
  ms para compensar. Pasan también sin cuerpos, de 6 a 147 por corrida. Solo con 10 000 mobs
  suben claro (de 853 a 1046 por corrida, p = 0.1).

## Dónde se va el tiempo (corrida con `--spans`, sin tope)

Por frame con paso (los frames en que corrió `FixedPostUpdate`). Media / p99 / peor, en ms.

| Configuración | Paso entero | Movimiento | Broadphase | Solve | Lo más caro del resto |
|---|---|---|---|---|---|
| Sin cuerpos | 0.013 / 0.033 / 0.069 | | | | |
| 1000 objetos | 0.081 / 0.32 / 0.39 | menos de 2 µs | menos de 2 µs | menos de 2 µs | prepare 0.014, wake 0.011 |
| 10 000 objetos | 0.203 / 0.71 / 0.84 | menos de 2 µs | menos de 2 µs | 0.029 | prepare 0.086, wake 0.035 |
| 1000 mobs | 0.580 / 1.03 / 1.35 | 0.189 | 0.151 | 0.098 | prepare 0.037, controlador 0.027 |
| 10 000 mobs | 3.77 / 5.06 / 8.88 | 1.141 | 0.964 | 1.193 (el peor 4.80) | prepare 0.152, controlador 0.104, dormir 0.054 |
| 1000 flechas | 0.277 / 0.66 / 0.94 | 0.003 | 0.007 | menos de 2 µs | flechas 0.128, prepare 0.041 |

Movimiento es `step_dynamic`. "Menos de 2 µs": `--spans` no escribe el sistema cuando su media no
llega a 2 µs y su peor paso a 50 µs.

- Pasos por frame: sin tope, de 0.014 a 0.021 (un paso cada 47 a 70 frames); con tope, 0.445 (uno
  cada 2.25 frames). Ningún frame tuvo dos pasos: en cada corrida hubo 1280 o 1281 frames con
  paso en 20 s, justo los 64 pasos por segundo. El tope de 3 pasos por frame no se usó.
- Dormidos: `stress` no imprime `PhysicsStats`, así que no hay conteo directo. Los objetos están
  todos dormidos al medir: su movimiento, broadphase y dormir no llegan a 2 µs, y 10 000
  despiertos costarían milisegundos. Los mobs no se duermen nunca (cambian de rumbo cada 48
  pasos); revisar si se duermen cuesta 0.054 ms con 10 000.
- `interpolate` corre en cada frame: 0.069 ms de media con 10 000 mobs, 0.010 con 1000 y casi
  nada con los objetos dormidos.
- Con tope de 144 el mismo paso cuesta más: 1000 mobs 0.98 ms contra 0.58 sin tope, 10 000
  objetos 0.29 contra 0.20, 10 000 mobs 3.94 contra 3.77. Probablemente la CPU baja el reloj
  entre frames.
- El peor frame con 1000 objetos sube 0.67 ms de media (p = 0.1) por una corrida de 5.74 ms. Sigue
  lejos de 6.94, y en la corrida con `--spans` los frames más lentos no tienen paso: son del
  render, como sin cuerpos.

## Con tope de 144 fps

Media de 3 corridas por configuración; el trabajo por frame sale de una corrida aparte con
`--spans`. Con tope la mitad de los frames dura apenas más de 6.944 ms por el propio limitador,
así que se cuentan los de más de 7.5 ms.

| Configuración | Frames > 7.5 ms por corrida | Frames mostrados por segundo | Trabajo por frame, p99 / peor ms |
|---|---|---|---|
| Sin cuerpos | 54 (6 a 147) | 141 (136 a 144) | 1.81 / 4.06 |
| 1000 objetos | 12 (4 a 24) | 143 | 2.57 / 4.62 |
| 10 000 objetos | 83 (9 a 147) | 140 | 1.96 / 4.18 |
| 1000 mobs | 45 (4 a 126) | 142 | 2.45 / 4.24 |
| 10 000 mobs | 954 (853 a 1046) | 96 (91 a 101) | 6.48 / 9.40 |
| 1000 flechas | 67 (4 a 109) | 140 | 2.15 / 4.47 |

- Ningún frame pasó de 10.4 ms (1.5 períodos) en ninguna corrida.
- Salvo 10 000 mobs, nada se distingue de la referencia (p de 0.2 a 1, rangos que se tocan).
- Con 10 000 mobs el frame con paso lleva unos 4 ms de trabajo, presenta tarde y pierde el
  permiso del compositor: un tercio de los frames se dibuja sin mostrarse. Se ve como 96 fps en
  una pantalla de 144 Hz.

## Para decidir

- Si un juego necesita miles de cuerpos despiertos a la vez, hay que bajar el paso de 10 000
  despiertos. Lo de entre cuerpos (broadphase y solve, 2.2 ms juntos aquí) sigue casi todo en un
  hilo, como ya decía la medición de P4.
- Antes de dar la regla por cumplida conviene medir el caso exacto del plan (9000 dormidos y 1000
  despiertos) a radio 16 y 32, quieto, volando y bajando, en las dos GPU. Para eso `stress`
  necesita mezclar tipos de cuerpos, y ayudaría que el resumen traiga `PhysicsStats` (dormidos,
  despiertos, pares).

## Cómo se midió

- Binarios de `bench-results/p7-2026-10-06/bin/`: `stress-new` (md5 3c23465c) y
  `stress-new-trace` (md5 70350440, con `--features trace`). Son la rama `measure/p7` en d8c67a0:
  master e895a11 más cambios del ejemplo `stress`, cuatro líneas del render y las herramientas. El
  código de física es el de master. `strings` confirma `--body-kind` en los dos y los spans solo
  en el de trace.
- RTX 5050 Laptop, Vulkan High, driver 595.91.07. Cada resumen dice que corrió en la RTX.
- `stress` en release, bosque (semilla 11566621), radio 16, radio vertical 8, cámara quieta, con
  luz, 20 s medidos después de 2 s de warmup, ventana 1280x720, present immediate (sale Mailbox).
  Los cuerpos aparecen cuando el mundo se asienta, en un cuadrado de 304 bloques (10 000) o de 96
  (1000) alrededor de la cámara. Todas las corridas pusieron todos los cuerpos pedidos.
- Corridas intercaladas: 3 vueltas sin tope con las configuraciones en otro orden cada vez, una
  con `--spans` por configuración y 3 con tope por configuración (la segunda y la tercera se
  agregaron al ver los frames de 8.7 ms), más una con tope y `--spans`. 48 corridas.
- `tools/wait-idle.sh 60` antes de cada una, nada compilando y ninguna otra `stress`. Ninguna salió
  ocupada ni se cruzó con otra. Carga al empezar entre 0.8 y 2.2.
- Significancia: test de permutación exacto sobre los valores de cada corrida, más los rangos.
  Con 3 corridas contra 3 el p más chico posible es 0.1. Las corridas con `--spans` son una por
  configuración, sin test.
- Las corridas, `run-phys.sh` y `ana_phys.py` (salida en `ana-phys.txt`) están en
  `bench-results/p7-2026-10-06/` (no está en git).
