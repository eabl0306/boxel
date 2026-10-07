# ADR 0017: con el límite en la frecuencia del monitor, el ritmo lo marca el monitor

- Estado: aceptado, con la revisión del 2026-10-07 (issue #2, ver "Revisión")
- Fecha: 2026-10-07
- Revisa: ADR 0013 (la espera a la compuerta) y el punto 5 del contrato del runner (ADR 0008)

## Contexto

Con `--fps-limit 144`, el límite por defecto de un juego, `stress` mostraba unos 3 frames por
segundo de 8.55 a 8.85 ms, casi siempre seguidos de uno de 5.2 ms, también en el bosque quieto sin
cuerpos ([cuevas-v1](../rendimiento/cuevas-v1-2026-10.md), [luz-p7](../rendimiento/luz-p7-2026-10.md),
[fisicas-p7](../rendimiento/fisicas-p7-2026-10.md)). Con 10 000 mobs despiertos se mostraban 96
frames por segundo de 144.

Lo medimos el 2026-10-07 en la RTX 5050 con Wayland (GNOME 50, Mailbox), con una traza del runner
y `WAYLAND_DEBUG` (el protocolo con sus tiempos). El panel lo maneja la Radeon 780M, así que cada
frame de la RTX cruza por PRIME y el compositor espera su fence (sincronización explícita).

1. Winit y mutter dicen que el panel es de 144.002 Hz, pero los frame callbacks llegan cada
   6.950 ms de mediana (143.88 Hz), con unos 0.5 ms de jitter para cada lado.
2. Con tope 144 la grilla de 6.944 ms queda de 0.3 a 1 ms después del callback: el enganche de
   ADR 0013 solo sigue a los callbacks que llegan tarde. Presentamos de 1 a 1.5 ms después del
   callback, y el compositor toma el frame si está listo unos 4.5 ms después del callback.
3. Aun así el compositor saltea de 0.2 a 5 refrescos por segundo, según la corrida. Los frames
   salteados se entregaron a tiempo, pero la GPU los terminó tarde: 3.7 ms de trabajo en vez de
   0.4 (en una corrida con la GPU cronometrada). Con tope, la RTX pasa casi todo el tiempo ociosa
   y baja de estado (P4 y P5, la memoria de 9001 a 810 MHz). Sin tope, con la GPU siempre ocupada,
   saltea 0.2 por segundo, y la 780M con tope, 0.1.
4. Cuando el compositor saltea un refresco, el frame callback llega un refresco tarde. El runner
   esperaba un cuarto de período, se rendía, corría una update sin presentar (8.7 ms) y la
   siguiente a los 5.2 ms. Esos eran los frames de 8.7 ms. El tirón que se ve es el refresco
   salteado; la espera no lo causa, pero gasta una update que no se muestra, y con frames pesados
   esa update atrasa a la que sí se presenta.
5. Con un límite apenas debajo del panel (143), la grilla barre todo el ciclo del refresco cada
   1.2 s. Cuando la update cae 4 a 5 ms después del callback, el frame llega tarde al compositor:
   se mostraban 121 a 123 frames por segundo.
6. Con vsync (Fifo), el driver de NVIDIA usa `wp_fifo_v1` y `wp_commit_timing_v1`: el frame
   callback llega un refresco más tarde que con Mailbox y la compuerta deja presentar uno de cada
   dos refrescos buena parte del tiempo (unos 107 por segundo, como decía ADR 0013).

## Decisión

- **El ritmo lo marca el monitor** cuando se cumplen tres cosas:
  - la compuerta sigue a los frame callbacks: Wayland, y los redraws de verdad esperan al
    compositor (un `RedrawRequested` que llega menos de 0.5 ms después de pedirlo no cuenta, y
    tras 8 seguidos se deja de seguir al monitor; así una ventana sin render, que recibe el redraw
    al instante, sigue con su grilla). Para empezar a seguirlo hacen falta 2 callbacks seguidos;
  - `FrameLimit` está en el 99 % de la frecuencia del monitor o más
    (`MonitorHandle::refresh_rate_millihertz`, que el runner consulta una vez por segundo, solo
    en Wayland; en las demás plataformas, una vez al crear la ventana);
  - los frame callbacks llegan a esa frecuencia: si la mediana de los últimos 16 intervalos se
    aleja más de un 10 % del período informado, se vuelve a la grilla (la ventana está en otro
    monitor que el que dice winit, o hay VRR);
  - la ventana está visible.
- Entonces el reloj es el frame callback: cada update corre apenas llega el suyo (no antes de
  medio período después del deadline anterior; los deadlines usan el período del monitor), y con
  la compuerta cerrada lo espera hasta un refresco más un cuarto de refresco, con ese cuarto de
  2 ms como máximo (1.25 refrescos a 144 Hz, 1.12 a 60 Hz), contados desde el deadline o desde el
  pedido del redraw si la update anterior terminó más tarde (lo que tarda el callback de un
  refresco salteado), sin espera activa: el callback despierta el bucle. Una compuerta abierta que
  ninguna update usó (no se presentó nada) no cuenta como callback: la update espera su deadline.
  Si no
  llega, la update corre sin presentar, como antes, y una ventana oculta baja a 30 Hz por el
  watchdog de 100 ms (ADR 0008).
- Arrancar justo en el callback le da al frame el mayor margen antes del límite del compositor.
  Contra una grilla pareja con el período del monitor, el compositor salteó un tercio menos de
  refrescos (593 contra 946 en 18 pares de corridas intercaladas, 12 pares a favor y 5 en
  contra). El costo es que el tiempo entre updates hereda el jitter de los callbacks: desvío de
  0.25 ms contra 0.08 de la grilla, sin contar los refrescos salteados.
- Un límite más bajo sigue con la grilla y la espera corta de ADR 0013. Sin límite (0) y en X11,
  nada cambia.
- `FramePacingInfo` (recurso de `boxel_window`) dice en cada update cuánto esperó la compuerta, si
  se rindió, cuándo llegó el último callback, la frecuencia del monitor y si siguió al monitor.
  `stress` lo anota por frame y lo resume en la sección Pacing.

## Consecuencias

Medido en la RTX 5050 con `stress`, bosque radio 16, Mailbox salvo donde dice vsync, 3 corridas
de 20 s por caso y versión, intercaladas (antes: 0e92379, que mide sin cambiar el ritmo; ahora:
3cfdc17). Promedios:

| Caso | Mostrados por segundo | Refrescos salteados | Frames de más de 7.5 ms | Updates sin presentar |
|---|---:|---:|---:|---:|
| Quieto, 144 | 139 -> 142 | 105 -> 30 | 107 -> 88 | 106 -> 0 |
| Vuelo, 144 | 139 -> 142 | 106 -> 34 | 107 -> 75 | 106 -> 2 |
| Quieto, 143 | 124 -> 141 | 380 -> 57 | 405 -> 96 | 383 -> 2 |
| Vuelo, 143 | 126 -> 140 | 322 -> 73 | 348 -> 114 | 323 -> 2 |
| Quieto, 150 | 144 -> 142 | 4 -> 40 | 34 -> 86 | 6 -> 2 |
| Vuelo, 150 | 141 -> 141 | 63 -> 63 | 93 -> 109 | 66 -> 2 |
| Quieto, 144, vsync | 107 -> 107 | 730 -> 725 | 731 -> 754 | 733 -> 6 |
| 10 000 mobs, 144 (2 corridas) | 95 -> 105 | | | |
| 780M, quieto, 144 | 143.7 -> 143.7 | 3 -> 3 | 4 -> 49 | 5 -> 1 |
| `voxel_world` (el juego, radio 12), 144 | 140.6 -> 141.9 | | | |
| `voxel_world`, 143 | 126 -> 142.6 | | | |

- Con 143 se dejan de perder unos 16 frames por segundo: la grilla ya no barre el ciclo del
  refresco.
- Con 144 desaparecen los frames de 8.7 ms seguidos de uno de 5.2, y el compositor saltea un
  tercio de los refrescos que salteaba antes. Los que quedan vienen de la GPU (de 0.2 a 6 por
  segundo según su estado, en rachas de uno cada unos 160 ms) y se ven como un frame de 13.9 ms.
  Los demás frames de más de 7.5 ms (2 a 3 por segundo) son callbacks que llegaron hasta 1 ms
  tarde, no tirones: sin contar los refrescos salteados, el desvío del tiempo entre updates baja
  de 0.47 a 0.25 ms.
- En la 780M, que maneja el panel, casi no había refrescos salteados: se muestra lo mismo, y los
  frames de 7.5 a 8 ms que aparecen son ese mismo jitter de los callbacks (desvío de 0.09 a
  0.25 ms). Es el costo de arrancar en el callback; en la RTX lo paga la mejora de arriba.
- Con 150 ya se arrancaba en el callback; queda igual dentro del ruido (en vuelo 63 contra 63; en
  quieto, 4 contra 40, con corridas de 5 a 79).
- Sin límite no cambia nada: 4570 contra 4535 updates por segundo quieto, 144 mostrados, ningún
  frame de más de 7.5 ms.
- Con vsync (Fifo) se muestra lo mismo que antes (unos 107 por segundo); ahora las updates bajan a
  esa cifra en vez de hacer 144 y no mostrar un cuarto. Arreglarlo pide no esperar los frame
  callbacks con Fifo, que tiene su propio freno, sin perder la protección de la ventana oculta:
  queda pendiente.
- Con menos updates por segundo (vsync, refrescos salteados) el mundo se pone al día más lento
  después de cruzar un borde, porque su trabajo tiene presupuesto por frame: en vuelo a 144 la
  puesta al día pasa de 19.5 a 24.2 ms, y con vsync de 17.7 a 29.4 ms. Agrandar ese presupuesto
  hacia la parte ociosa del frame casi no ayuda a 144 Hz
  ([ritmo-2026-10](../rendimiento/ritmo-2026-10.md)).
- Las updates siguen corriendo en `about_to_wait`, nunca dentro de `RedrawRequested`, y nunca
  esperan un callback sin límite de tiempo, así que la simulación no se congela con la ventana
  oculta. Lo que cambia del contrato de ADR 0008 es que, al seguir al monitor, el callback decide
  cuándo corre la update.
- Con un límite arriba de la frecuencia del monitor, en Wayland las updates bajan a la del
  monitor: antes se hacían updates de más que no se mostraban. Para correr más updates que el
  monitor queda el límite 0.

## Alternativas descartadas

- Agrandar o achicar la espera de ADR 0013: no cambia los refrescos salteados, y con el límite
  debajo del panel la grilla igual barre el ciclo del refresco.
- No esperar nunca a la compuerta: con 0.5 ms de jitter en los callbacks se pierden refrescos que
  sí iban a llegar (ADR 0013 lo midió: 113 a 126 mostrados de 144).
- Correr una update sin presentar en el refresco salteado (lo de antes): la pantalla muestra lo
  mismo, se gasta una update que no se ve, y con frames pesados atrasa la siguiente.
- Una grilla pareja con el período del monitor que solo espera el callback cuando llega tarde
  (la versión que medimos primero): frames parejos, pero un tercio más de refrescos salteados, y
  depende de que winit no informe una frecuencia más baja que la real (si no, la grilla se corre
  contra el compositor como pasaba con 143).
- Medir la frecuencia con los callbacks en vez de preguntarle a winit: los callbacks solo llegan
  después de presentar, así que con un límite bajo medirían el límite y no el monitor. La
  revisión sí los mide, pero solo para comprobar lo que dice winit, y solo los intervalos de un
  redraw pedido menos de medio refresco después del callback anterior, que sí miden el monitor.
- Tener la GPU ocupada para que no baje de estado: gasta batería para esconder un problema del
  driver.

## Revisión

Una revisión adversaria del código (issue #2) encontró estos problemas. Cada arreglo tiene su
test sobre `RunnerCore`.

- **Compuerta abierta sin usar (alto).** Al seguir al monitor, si nada presentaba (sin cámara, un
  menú, la GPU sin estar lista, la superficie reconstruyéndose), la compuerta quedaba abierta y cada
  update corría medio refresco después de la anterior: 288 updates por segundo a 144 Hz, sin
  respetar el límite. Ahora solo cuenta un callback que llegó después de la última update; si
  no, la update espera su deadline.
- **Update lenta (medio).** La espera máxima se contaba desde el deadline, así que una update que
  terminaba más de 1.25 refrescos tarde (un juego por debajo de unos 64 fps) no esperaba su
  callback y la siguiente corría sin presentar. Ahora se cuenta desde el pedido del redraw si es
  más tarde.
- **Frecuencia informada distinta de la real (medio).** Winit da la frecuencia del primer monitor
  donde entró la ventana; con varios monitores o VRR puede no ser la de los callbacks. Ahora se
  comprueba con la mediana de los intervalos entre callbacks, como dice la decisión.
- **Menores.** La frecuencia se consulta cada segundo solo en Wayland (en macOS cuesta un viaje
  al servidor de ventanas). Hacen falta 2 callbacks seguidos para empezar a seguir al monitor. Un
  cambio de límite mientras se espera la compuerta ya no arrastra la espera anterior en
  `FramePacingInfo`. El texto decía "un refresco y un cuarto", pero el cuarto tiene tope de
  2 ms: a 60 Hz son 1.12 refrescos.
- El test con ventana (`frame_limit_window`) no puede ver `display_locked` en ningún caso: en
  Wayland la ventana no se mapea y no hay frecuencia, y en X11 la compuerta no sigue callbacks.
  Lo cubren los tests unitarios del runner.
