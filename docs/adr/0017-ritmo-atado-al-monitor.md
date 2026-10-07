# ADR 0017: con el límite en la frecuencia del monitor, el ritmo lo marca el monitor

- Estado: aceptado
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
    al instante, sigue con su grilla);
  - `FrameLimit` está en el 99 % de la frecuencia del monitor o más
    (`MonitorHandle::refresh_rate_millihertz`, que el runner consulta una vez por segundo);
  - la ventana está visible.
- Entonces el reloj es el frame callback: cada update corre apenas llega el suyo (no antes de
  medio período después del deadline anterior; los deadlines usan el período del monitor), y con
  la compuerta cerrada lo espera hasta un refresco y un cuarto después del deadline (lo que tarda
  el callback de un refresco salteado), sin espera activa: el callback despierta el bucle. Si no
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

Medido en la RTX 5050, bosque radio 16 quieto, Mailbox, 4 corridas de 20 s por versión,
intercaladas:

| | Límite 144, antes | 144, ahora | Límite 143, antes | 143, ahora |
|---|---:|---:|---:|---:|
| Frames mostrados por segundo | 137 (136 a 140) | 140 (138 a 143) | 125 (123 a 129) | 142 (139 a 144) |
| Refrescos salteados en 20 s | 130 (86 a 158) | 74 (8 a 110) | 348 (274 a 392) | 38 (3 a 100) |
| Frames de más de 7.5 ms | 133 | 107 | 372 | 78 |
| Updates que corrieron sin presentar | 132 | 2 | 349 | 2 |

- Con 143 se dejan de perder 20 frames por segundo: la grilla ya no barre el ciclo del refresco.
- Con 144 los frames de 8.7 ms seguidos de 5.2 ms desaparecen. Los refrescos salteados quedan
  (según el estado de la GPU, de 0.2 a 6 por segundo, cada unos 160 ms en las rachas malas) y se
  ven como un frame de 13.9 ms. Los frames de más de 7.5 ms que no son refrescos salteados son
  callbacks que llegaron hasta 1 ms tarde (el jitter del compositor), no tirones.

- Las updates siguen corriendo en `about_to_wait`, nunca dentro de `RedrawRequested`, y nunca
  esperan un callback sin límite de tiempo, así que la simulación no se congela con la ventana
  oculta. Lo que cambia del contrato de ADR 0008 es que, al seguir al monitor, el callback decide
  cuándo corre la update.
- Con un límite arriba de la frecuencia del monitor, en Wayland las updates bajan a la del
  monitor: antes se hacían updates de más que no se mostraban. Para correr más updates que el
  monitor queda el límite 0.
- Los refrescos que saltea el compositor porque la RTX terminó tarde no desaparecen: ahora se ven
  como un frame de 13.9 ms (lo que vio la pantalla) en vez de uno de 8.7 y otro de 5.2, sin la
  update de más.

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
  después de presentar, así que con un límite bajo medirían el límite y no el monitor.
- Tener la GPU ocupada para que no baje de estado: gasta batería para esconder un problema del
  driver.
