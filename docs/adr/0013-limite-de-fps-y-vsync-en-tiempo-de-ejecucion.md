# ADR 0013: límite de FPS en los runners y vsync cambiable en tiempo de ejecución

- Estado: aceptado
- Fecha: 2026-10-05

## Contexto

En M0 el runner de winit usaba `ControlFlow::Poll`: el bucle hacía todos los updates que podía.
Medido en la RTX 5050 con Wayland: unas 11 000 a 15 000 updates por segundo y 150 % de un núcleo
de CPU, para mostrar solo unos 140 frames por segundo. El overlay F3 mostraba esas updates como
"FPS". El modo de presentación se elegía una sola vez al seleccionar la GPU (FIFO por defecto,
ADR 0008).

Cualquier juego hecho con boxel necesita una opción "límite de FPS" y otra de vsync en su menú,
y las dos tienen que cambiar sin reiniciar.

## Decisión

- **`boxel_app::FrameLimit { max_fps }`**: recurso público, en el prelude. Por defecto 144; 0 es
  sin límite. Los dos runners lo leen antes de cada frame, así que un cambio hecho por cualquier
  sistema vale desde el frame siguiente. `WindowPlugin` lo inserta con `init_resource` (gana el
  valor que inserte la app). El runner headless lo usa en lugar de su `wait` cuando existe.
- **Ritmo por deadlines (`FramePacer`)**: el frame siguiente vence un período después de que
  venció el anterior, no después de que terminó, así que despertar tarde no acumula error. Un frame
  que empieza más de un período tarde reinicia la grilla: no hay ráfagas para recuperar. La espera
  gruesa duerme (`WaitUntil` en winit, `sleep` en headless) y los últimos 300 µs ceden el hilo en
  un bucle (`yield_now`), porque los timers del sistema despiertan tarde. Sin límite: `Poll`.
- **Compuerta y frame callbacks (Wayland)**: si un frame vence con la compuerta de presentación
  cerrada, el runner espera el frame callback hasta un cuarto del período (2 ms como máximo). Si
  llega en esa ventana, la grilla se reinicia en ese momento: las updates se enganchan al reloj del
  compositor. Sin esto, con el límite igual a la frecuencia del panel, la grilla deriva contra el
  compositor y muchas updates no presentan.
- **Ventana oculta**: el tope de 30 Hz sigue igual (el período es el mayor entre el límite y
  33 ms), así que la prueba de minimizar no cambia.
- **`boxel_render::PresentSettings { mode }`**: recurso público para el vsync. Un cambio
  reconfigura la superficie con el mismo dispositivo en el siguiente `Render` (sin dispositivo
  nuevo ni resubir chunks). `RenderInfo::present_mode` dice qué modo quedó.
- **Vsync apagado por defecto**: `PresentModePref::NoVsync` pide `Mailbox`, si no `Immediate`, si
  no `Fifo` (con una nota en el log). Así manda el límite de FPS y no el monitor. `--present`,
  `BOXEL_PRESENT` o el archivo de configuración reemplazan el valor una vez, al elegir la primera
  GPU.
- Overlay F3: frames mostrados y updates por separado, límite, vsync y modo. Teclas F5 (ciclo 30,
  60, 144, 240, sin límite) y F6 (vsync) opcionales en `DebugOverlayPlugin::frame_keys`; apagadas
  por defecto para no ocupar teclas de los juegos.

## Consecuencias

Medido en la máquina de desarrollo (panel de 144 Hz, Wayland, radio 12, mundo cargado):

| Caso (RTX 5050, Wayland) | Updates/s | Mostrados/s | CPU (100 % = un núcleo) |
|---|---|---|---|
| 144, vsync apagado (Mailbox) | 143.6 | 136 a 143 | 20 % |
| Sin límite (Mailbox) | 11 000 a 16 000 | 139 a 143 | 150 % |
| 60 | 60.0 | 60.0 | 9 % |
| 144, vsync (Fifo) | 143.6 | unos 107 | 17 % |

- Sin render el límite se cumple con precisión: con una ventana real, 60.00 y 144.00 updates
  por segundo en Wayland y en X11 (`just test-window`).
- Con render en Wayland, la espera del frame callback cuesta un poco de precisión: las updates
  pueden quedar hasta un 2 % abajo del límite (119 con 120, 127.5 con 130, 143.6 con 144, 238.6
  con 240; 60 y 100 exactos). Los frames mostrados también pueden quedar unos pocos por ciento
  abajo aunque el límite esté por debajo del panel (111 a 119 con 120, 119 a 126.5 con 130).
  Sin la espera las updates son exactas, pero se muestran solo 100 a 109 por segundo con 120 a
  144: el canje vale la pena.
- La CPU baja de 150 % a 20 % de un núcleo con el límite por defecto. Los 300 µs de espera activa
  cuestan entre 3 y 4 % de un núcleo a 144 fps.
- En Wayland, con vsync (Fifo), NVIDIA y RADV muestran unos 107 frames por segundo de 144, con o
  sin límite: es la combinación de Fifo con la compuerta, no el limitador. Sin vsync se ven 136 a
  143. GL (radeonsi) solo ofrece Fifo.
- En X11 no hay compuerta de frame callbacks: sin vsync se muestra lo que diga el límite (240 con
  240); con vsync, la presentación bloquea y las updates bajan a la frecuencia del monitor.
- La carga de chunks está atada a los frames (`voxeles.md`, 7.2): con 144 fps, radio 12 tarda
  6.5 s en cargar (antes 0.6 s), radio 16 tarda 11 s y radio 32, 41 s (antes 1.7 s). Hay que
  desatarla (plan de `voxeles.md`, paso 3).
- Las pruebas automáticas siguen pasando y tardan más: `--frames 120` pasa de 0.5 s a 1.5 s.

## Alternativas descartadas

- Dormir en un sistema al final del frame (como `bevy_framepace`): bloquea el hilo principal
  dentro de `App::update`, sin procesar eventos, y no sabe de la compuerta ni de la ventana oculta.
- Dejar `Poll` y limitar solo con vsync: el vsync de Wayland no limita las updates (la compuerta
  evita que Fifo bloquee) y en un portátil gasta un núcleo entero.
- Dormir con `sleep` hasta el deadline exacto: la latencia del timer (50 µs o más) hace que cada
  frame llegue tarde; la grilla corrige el promedio pero no cada frame.
- Vsync encendido por defecto: con la compuerta, en Wayland muestra menos frames que Mailbox y en
  X11 ata las updates al monitor.
