# ADR 0022: el mundo trabaja entre frames

- Estado: aceptado (issue #3; medido con la máquina libre, ver "Consecuencias")
- Fecha: 2026-10-08
- Revisa: el punto 5 del contrato del runner (ADR 0008) y la espera de ADR 0013 y ADR 0017 (los
  dos runners corren pasos cortos mientras esperan). Quita el crecimiento de presupuestos de
  7281b1d.

## Contexto

Con tope 144, cada update termina antes del deadline y el runner duerme unos 4 ms de cada 6.94.
El mundo solo trabaja dentro de `Update`: en `Dispatch` recoge los jobs terminados y lanza otros,
y repite mientras lleguen resultados, con un tope de 2 ms. Un resultado que llega después espera
al frame siguiente. La cadena de un chunk tiene varias etapas (generar, caminar la columna del
cielo, mallar, publicar) y cada una puede esperar un frame entero.

- Issue #3, RTX 5050, radio 16: después de cruzar un borde, el mundo tardaba 24.8 ms en ponerse
  al día con tope y 17.1 sin tope en la superficie, y 34.9 contra 27.3 bajo tierra.
- Esos 17.1 y 27.3 ms eran con tope 144 y de antes de la luz
  ([luz-fisicas](../rendimiento/luz-fisicas-2026-10.md)). Sin tope y con luz (e895a11): 12.9 ms en
  la superficie, 21.4 bajo tierra y 25.6 bajando en picado.
- Primer intento (7281b1d): agrandar `interest_budget` y `job_budget` hasta el deadline, dejando
  medio período libre. A 144 Hz no cambió nada (superficie 29.9 -> 28.5 ms, bajo tierra 37.6 ->
  37.8, ruido). Con un margen de 1.5 ms la superficie bajó a 26.6 ms pero se saltearon más
  refrescos (113). El límite no es el presupuesto sino la cuantización en frames.

## Decisión

### Schedule `Idle` (boxel_app)

- Está fuera del frame, como `Shutdown`, con el ejecutor de un hilo. Un runner que espera un
  límite lo corre en pasos cortos entre updates (`App::run_idle`), después de que el frame se
  presentó (el render corre en `Render`, antes de que termine la update).
- Cada paso recibe un recurso `IdleStep` con la hora a la que debe terminar (`until`). Los
  sistemas no bloquean y contestan con `IdleStep::want`:
  - `Ready`: hay más trabajo ya, otro paso enseguida;
  - `Waiting`: hay trabajo en vuelo en otros hilos, otro paso en 0.25 ms (`IDLE_POLL`);
  - `Done`: nada hasta la próxima update.
- `run_idle` no limpia los trackers de cambios: lo que un paso cambia se ve como cambiado en la
  update siguiente, y los mensajes que escribe los leen los sistemas de esa update.
- La ventana de pasos termina 0.7 ms (`IDLE_MARGIN`) antes del margen de espera activa del
  deadline: 1 ms antes del deadline con el margen por defecto (0.3 ms). Un paso dura como mucho
  0.5 ms (`IDLE_STEP`) y no se empieza ninguno con menos de 0.2 ms libres (`MIN_IDLE_STEP`).
- `IdleInfo`: los dos runners lo publican antes de cada update, junto a `FrameDeadline`. Dice el
  tiempo y los pasos del hueco anterior, cuánto se pasó de la ventana el peor paso de ese hueco,
  y cuántos pasos se pasaron desde que arrancó el runner.
- `IdleWanted` (opcional): los runners lo limpian antes de cada update y abren la ventana solo si
  un sistema de esa update pidió pasos (`IdleWanted::want`; los pedidos se suman). Sin el
  recurso, la ventana se abre después de cada update con lugar, como al principio.

### Runner winit

- Abre la ventana solo si la app tiene sistemas en `Idle`, la update pidió pasos (si hay
  `IdleWanted`), está en modo `Continuous` y la update termina con espera (`FrameWait::Sleep`).
  La ventana termina `IDLE_MARGIN` antes de lo que ocurra primero: el deadline menos la espera
  activa, o la hora a la que el runner se despertaría de todos modos. Siguiendo al monitor con la
  compuerta cerrada, eso es 1 ms antes del deadline. Si un frame callback llegó temprano, es
  0.7 ms antes del arranque más temprano de la update.
- **Primero `Poll`.** La iteración de la update no corre ningún paso: pone `ControlFlow::Poll`
  en vez de su espera. Así winit vacía la conexión de Wayland y entrega un `RedrawRequested` que
  llega enseguida (X11, o una ventana de Wayland que no presenta) antes del primer paso, con la
  misma marca de tiempo que antes. `QUICK_GATE` y las reglas de ADR 0017 ven los mismos tiempos.
- Cada iteración siguiente de la espera corre un paso. Entre pasos se atienden los eventos, la
  compuerta y el deadline, así que un paso nunca retiene una update: si llega el frame callback,
  la update corre en la iteración siguiente. Un evento que llega durante un paso se atiende
  cuando el paso termina, hasta 0.5 ms tarde. Como la ventana termina 1 ms antes del deadline,
  eso solo toca a los callbacks que llegan más de 1 ms antes de tiempo.
- No cambia nada sin sistemas en `Idle` o sin pedido (ninguna iteración de más), sin límite
  (tampoco con la ventana oculta: la espera de 33 ms no es un límite), en modo `Reactive`, ni
  después de una update que terminó tarde. Con límite y la ventana oculta, la ventana de pasos
  sigue el período de 33 ms.

### Runner headless

Igual, después de cada update con ritmo que pidió pasos: pasos seguidos mientras contestan
`Ready`, y una espera de 0.25 ms entre pasos `Waiting`. Nada con `RunMode::Once` ni sin ritmo.

### Mundo (`WorldSettings::idle_work`, encendido por defecto)

- Un sistema en `Idle`, seguido de `publish_data_changes`. Si hay trabajo, hace rondas hasta el
  `until` o hasta una ronda que no encuentra nada:
  - recoge los jobs terminados;
  - lanza jobs de la cola mientras haya lugar;
  - si quedan al menos 0.3 ms, aplica un trozo del trabajo de interés que dejaron los frames
    (posiciones, prioridades viejas, caché de luz), de 0.25 ms como máximo.
- Sin trabajo para hacer ya solo lee el canal y el scheduler, sin marcar el mapa como cambiado.
- Al final de `Collect` pide pasos solo si uno encontraría trabajo: resultados por recoger, jobs
  por lanzar, trabajo de interés o jobs en vuelo. Entre ese punto y la update siguiente nada más
  mueve los jobs, así que un frame que no pide solo se ahorra un paso que contestaría `Done`.
- Lo que queda en el frame, para que el resultado del mundo no dependa de los tiempos:
  - planear los cambios de los viewers;
  - las ediciones y `BlockChanged`;
  - el tope de jobs en vuelo y el reloj de los lotes de edición (por frame);
  - la edad de los lotes;
  - publicar las mallas del streaming, en `Collect` y con su tope por frame;
  - refrescar `WorldStats` (en `Last`).
- Entre frames el mundo puede escribir:
  - `Remove` (descargas, mallas que quedaron vacías o que ya no se quieren);
  - los `ChunkDataEvent` de lo que cargó o descargó;
  - rara vez, los `Upsert` de un lote de edición cuyo último miembro pendiente se fue (se
    descargó o quedó sin malla), igual que en un frame.
- Todo eso lo leen los sistemas del frame siguiente. Apagado, el mundo trabaja como antes y no
  pide pasos nunca: el runner espera igual que en main (sin `Poll` ni paso), por construcción.

### Otros

- Se quita `FrameBudget` (7281b1d): los pasos del frame usan otra vez los presupuestos de
  `WorldSettings`, tal cual. `FrameDeadline` queda como diagnóstico, sin consumidores en el
  motor.
- `stress` y `headless_world`: `--idle-work on|off` para comparar el mismo binario con y sin
  trabajo entre frames. `stress` anota por frame los pasos ociosos y cuándo terminó el último
  job, y la puesta al día suma los frames que tardó y el retraso del trabajo: del inicio del
  frame del cruce al fin del último job, sin redondear a frames. `tools/bench_report.py` muestra
  `idle work` pero no empareja por ella.

## Consecuencias

Medido con la máquina libre, el mismo binario con tope 144 (trabajo entre frames apagado y
encendido) y sin tope. RTX 5050, radio 16, overworld con cuevas, 3 a 6 corridas por caso. Una
corrida que se cruzó con una compilación u otro test se repitió. Detalle en
[entre-frames-2026-10](../rendimiento/entre-frames-2026-10.md).

| Puesta al día, media (rango) | Tope, apagado | Tope, encendido | Sin tope |
|---|---:|---:|---:|
| Superficie, vuelo a 64 | 29.3 ms (25.5 a 32.1) | 16.5 ms (15.1 a 19.4) | 18.3 ms (14.2 a 29.8) |
| Bajo tierra, vuelo a 64 | 39.0 ms (38.2 a 40.3) | 23.2 ms (22.7 a 23.9) | 25.5 ms (23.9 a 28.2) |
| Bajando en picado a 64 | 35.0 ms (33.6 a 36.0) | 31.2 ms (29.3 a 34.4) | 36.3 ms (31.6 a 46.7) |

| Salto de 64 chunks a y = -1000, `headless_world`, radio 16 | Tope 144, apagado | Tope 144, encendido | Sin límite |
|---|---:|---:|---:|
| Hasta que la zona nueva está lista | 0.40 a 0.41 s | 0.38 s | 0.43 a 0.47 s |

- Con tope y trabajo entre frames la puesta al día queda igual o por debajo de la misma corrida
  sin tope en los tres casos. En la superficie la mediana sin tope es 14.7 ms: la de con tope
  queda 1 ms arriba porque se mide en frames, que empiezan en la grilla de 6.94 ms. El retraso
  del trabajo, que no se redondea, es 13.8 ms con tope y 18.1 sin tope.
- Contra los números de la issue: superficie (~17) y bajo tierra (~27) se cumplen. El picado
  (~25) no, pero sin tope este binario tampoco llega: tarda 36 ms. El mundo pesa más que cuando
  se midieron esos 25 ms.
- La primera medición, con la máquina cargada, daba bajo tierra 31 a 41 ms y picado 30 a 36. La
  carga pegaba más en las corridas con tope: de las 9 primeras, 4 se cruzaron con una compilación
  de otro agente, y ninguna sin tope. Una corrida con tope deja la CPU casi libre, y ahí arranca
  el trabajo de los demás.
- Sin límite no cambia nada: ningún runner corre `Idle`, tampoco con la ventana oculta.
- Ritmo, apagado contra encendido: los frames mostrados por segundo no cambian (superficie 138.9
  contra 139.6, bajo tierra 141.5 contra 141.2, picado 140.6 contra 139.7, dentro del ruido). Los
  frames de más de 7.5 ms ya estaban apagado (de 57 a 142 por corrida de 15 s). En el picado
  suben de 71 a 125 de media, con rangos que casi se tocan (58 a 93 contra 77 a 181): hay que
  vigilarlo. Con vsync no se midió.
- Pasos fuera de la ventana: de 0 a 7 por corrida, el peor de 0.34 ms, dentro del margen de
  0.7 ms. Ningún paso retrasó una update.
- El hilo principal trabaja en un tiempo en el que antes dormía: 0.04 a 0.06 ms por frame en
  promedio y 1.7 a 2.5 ms en el p99 (los huecos de los cruces). Es el mismo trabajo, hecho antes.
- Las mallas del streaming llegan juntas al `Collect` siguiente (hasta 512 por frame): riesgo de
  subidas más grandes por frame. Si el picado confirma más refrescos salteados, probar
  `max_mesh_events_per_frame = 256`.
- No cambian: el contrato de un mensaje por job, `ResultToken`, el tope automático por frame, la
  lógica del scheduler, el orden de `Main`, `Time` y `FixedTime`, la física (no corre en `Idle`),
  el render (no corre en `Idle`) y lo que dicen los campos que ya tenía `stress`.

### Ventana solo con trabajo (`IdleWanted`)

- Al principio la ventana se abría después de cada update, también sin nada que hacer: primero
  `Poll` y después un paso que contesta `Done`. En el bosque (radio 16, vuelo a 32, tope 144,
  RTX 5050) el ritmo salía un poco peor también con el trabajo apagado.
- Diagnóstico, 46 rondas emparejadas, apagado contra el mismo binario sin ventana: más corridas
  en el modo alto de refrescos salteados (30 contra 18, McNemar p = 0.023), +19 salteados por
  20 s (p = 0.018) y 1 mostrado por segundo menos (p = 0.018). Encendido no se distinguía de
  main (12 rondas, p = 0.44). El paso cuesta 6.5 us de media: lo que pesa es abrir la ventana.
  Datos en `bench-results/issue3/diag-forest/` (`pairs.txt`, `modes.txt`).
- Arreglo: el mundo pide pasos solo cuando le queda trabajo. Apagado ahora es igual a main por
  construcción (ningún paso, la espera de siempre). Encendido, en el bosque solo el 1 % de los
  frames abre ventana (antes todos).
- Prueba rápida (una corrida de 10 s por lado contra 48a0427, ver
  [puesta-al-dia](../rendimiento/puesta-al-dia-2026-10.md)): sin regresiones, y la puesta al día
  igual.

## Alternativas descartadas

- Presupuestos más grandes dentro de la update (7281b1d): medido, no ayuda a 144 Hz.
- Encadenar los jobs en los workers (generar, caminar y mallar sin pasar por el hilo principal):
  quita saltos con y sin tope, pero cambia el comportamiento sin tope, saltea el tope automático
  de jobs y agrega una capa de concurrencia sobre la máquina de estados y el contrato de un
  mensaje por job (estimado en 1000 a 1400 líneas).
- Un despertador de resultados (un `EventLoopProxy` o despertar el hilo) en vez de sondear cada
  0.25 ms: ahorra unos 0.1 ms por salto, invisible en una métrica de 6.94 ms, y cruza tres crates.
- Publicar mallas entre frames: no se ve antes (el render del frame siguiente las lee igual) y
  suma estado para repartir el tope por frame.
- Correr el primer paso en la misma iteración de la update: retrasa la marca de un redraw que
  llega enseguida y lo puede confundir con un frame callback.
- Un tope automático de jobs más chico con tope de fps (medido: el tope calculado para frames de
  2 ms en vez de 6.94). Con tope, la cola en los workers es mucho más honda que sin tope (370 a
  880 jobs en vuelo contra 110 a 290), pero achicarla a unos 250 no acelera la puesta al día
  (superficie 16.5, bajo tierra 23.9, picado 31.4 ms).

## Pendiente, solo después de medir

- Interés ascendente primero: en cada fila, crear antes que descargar. Cambia también sin tope.
- Presupuestos de la update más chicos con el trabajo entre frames encendido, si los frames de
  los cruces suman refrescos salteados.
- El despertador de resultados, si el retraso del trabajo queda más de 0.5 ms por salto arriba
  del de sin tope.
- Medir con vsync, a 60 fps y en la 780M. Repetir el picado: si los frames de más de 7.5 ms suben
  de verdad, probar `max_mesh_events_per_frame = 256`.
