# ADR 0022: el mundo trabaja entre frames

- Estado: aceptado; falta la medición completa (issue #3)
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

### Runner winit

- Abre la ventana solo si la app tiene sistemas en `Idle`, está en modo `Continuous` y la update
  termina con espera (`FrameWait::Sleep`). La ventana termina `IDLE_MARGIN` antes de lo que
  ocurra primero: el deadline menos la espera activa, o la hora a la que el runner se despertaría
  de todos modos. Siguiendo al monitor con la compuerta cerrada, eso es 1 ms antes del deadline.
  Si un frame callback llegó temprano, es 0.7 ms antes del arranque más temprano de la update.
- **Primero `Poll`.** La iteración de la update no corre ningún paso: pone `ControlFlow::Poll`
  en vez de su espera. Así winit vacía la conexión de Wayland y entrega un `RedrawRequested` que
  llega enseguida (X11, o una ventana de Wayland que no presenta) antes del primer paso, con la
  misma marca de tiempo que antes. `QUICK_GATE` y las reglas de ADR 0017 ven los mismos tiempos.
- Cada iteración siguiente de la espera corre un paso. Entre pasos se atienden los eventos, la
  compuerta y el deadline, así que un paso nunca retiene una update: si llega el frame callback,
  la update corre en la iteración siguiente. Un evento que llega durante un paso se atiende
  cuando el paso termina, hasta 0.5 ms tarde. Como la ventana termina 1 ms antes del deadline,
  eso solo toca a los callbacks que llegan más de 1 ms antes de tiempo.
- No cambia nada sin sistemas en `Idle` (ninguna iteración de más), sin límite, en modo
  `Reactive`, ni después de una update que terminó tarde. Con la ventana oculta la ventana de
  pasos sigue el período de 33 ms.

### Runner headless

Igual, después de cada update con ritmo: pasos seguidos mientras contestan `Ready`, y una espera
de 0.25 ms entre pasos `Waiting`. Nada con `RunMode::Once` ni sin ritmo.

### Mundo (`WorldSettings::idle_work`, encendido por defecto)

- Un sistema en `Idle`, seguido de `publish_data_changes`. Si hay trabajo, hace rondas hasta el
  `until` o hasta una ronda que no encuentra nada:
  - recoge los jobs terminados;
  - lanza jobs de la cola mientras haya lugar;
  - si quedan al menos 0.3 ms, aplica un trozo del trabajo de interés que dejaron los frames
    (posiciones, prioridades viejas, caché de luz), de 0.25 ms como máximo.
- Sin trabajo para hacer ya solo lee el canal y el scheduler, sin marcar el mapa como cambiado.
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
- Todo eso lo leen los sistemas del frame siguiente. Apagado, el mundo trabaja como antes.

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

Medición rápida en la máquina compartida con otros dos agentes (carga 2 a 7, CPU ocupada 17 a
40 % antes de varias corridas), detalle en
[entre-frames-2026-10](../rendimiento/entre-frames-2026-10.md). La medición completa queda para
otra pasada.

| Caso (RTX, radio 16, tope 144) | Puesta al día, apagado | Puesta al día, encendido | Retraso del trabajo, apagado -> encendido |
|---|---:|---:|---:|
| Superficie, vuelo a 64 (4 corridas) | 28.6 a 40.4 ms (media 33.3) | 15.3 a 25.0 ms (media 19.2) | 29.5 -> 16.8 ms |
| Bajo tierra, vuelo a 64 (2) | 37.1 y 76.4 ms | 31.1 y 40.8 ms | 53.5 -> 32.5 ms |
| Bajando en picado a 64 (2) | 48.0 y 75.9 ms | 29.6 y 35.7 ms | 57.3 -> 28.5 ms |

| Salto de 64 chunks, `headless_world`, radio 16 (sin GPU) | Apagado | Encendido |
|---|---:|---:|
| 144 Hz | 0.25 a 0.31 s | 0.10 a 0.11 s |
| 60 Hz | 0.65 a 0.67 s | 0.12 s |
| Sin límite | 0.10 a 0.14 s | 0.10 a 0.11 s (igual, ruido) |

- La puesta al día con tope baja a la mitad en la superficie y se acerca a los valores sin tope
  (12.9 ms en la superficie). Medida en frames sigue redondeada al período: el mundo termina
  antes, pero el frame que lo registra empieza en la grilla.
- A 60 Hz gana más: el hueco es de unos 12 ms.
- Sin límite no cambia nada: ningún runner corre `Idle`.
- El hilo principal trabaja en un tiempo en el que antes dormía: 0.04 a 0.07 ms por frame en
  promedio, 1.5 a 2.5 ms en el p99, y de 2 a 7 ms (hasta 14 pasos) en los huecos de los cruces. Es el
  mismo trabajo, hecho antes.
- Pasos que terminan después de la ventana: casi siempre de 0.02 a 0.25 ms, que cubre el margen.
  En 3 de las 8 corridas con la máquina cargada hubo uno de 3.1 a 4.1 ms, que alargó ese frame.
  En una corrida instrumentada ningún resultado ni arranque de job pasó de 0.15 ms, así que lo
  más probable es que el sistema operativo haya sacado al hilo de la CPU. La medición completa
  tiene que mirar `overrun_max` con la máquina libre.
- Las mallas del streaming llegan juntas al `Collect` siguiente (hasta 512 por frame): riesgo de
  subidas más grandes por frame. Si sube el número de refrescos salteados, probar
  `max_mesh_events_per_frame = 256`.
- No cambian: el contrato de un mensaje por job, `ResultToken`, el tope automático por frame, la
  lógica del scheduler, el orden de `Main`, `Time` y `FixedTime`, la física (no corre en `Idle`),
  el render (no corre en `Idle`) y lo que dicen los campos que ya tenía `stress`.

### Criterio propuesto para cerrar la issue

Que decida el dueño: con tope y trabajo entre frames, una puesta al día menor o igual que el
mayor entre la meta de la issue y la misma corrida sin tope más un período, un retraso del
trabajo parecido al de sin tope, ningún paso fuera de la ventana con la máquina libre, y sin
empeorar el ritmo, sin tope ni con vsync.

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

## Pendiente, solo después de medir

- Interés ascendente primero: en cada fila, crear antes que descargar. Cambia también sin tope.
- Presupuestos de la update más chicos con el trabajo entre frames encendido, si los frames de
  los cruces suman refrescos salteados.
- El despertador de resultados, si el retraso del trabajo queda más de 0.5 ms por salto arriba
  del de sin tope.
