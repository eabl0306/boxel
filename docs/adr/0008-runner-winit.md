# ADR 0008: runner propio sobre winit 0.30 con presentación gobernada por frame callbacks

- Estado: aceptado (revisado 2026-10-05, 2026-10-07 y 2026-10-08)
- Fecha: 2026-10-04

## Contexto

`bevy_winit` arrastra crates de Bevy. winit 0.30.13 es la estable (0.31 en beta). GL en Wayland
exige el display handle al crear la instancia de wgpu.

El borrador evitaba congelar la simulación con la ventana oculta usando `WindowEvent::Occluded`,
pero winit 0.30.13 nunca envía ese evento en Wayland ni en Windows (verificado en su código). Con
FIFO, presentar en una superficie oculta puede bloquear el hilo principal. Medido con una ventana
minimizada 2 s: presentando en cada update, GL se congeló (0 updates) y Vulkan quedó en ~105
updates/s. Además, la ventana como recurso `Send` invitaba a llamar métodos de winit desde
sistemas paralelos, que en macOS y Windows es un deadlock.

## Decisión

- `boxel_window`: `WindowPlugin` crea el `EventLoop` en `build`, inserta `DisplayHandle`
  (`OwnedDisplayHandle`) y el runner. El runner implementa `ApplicationHandler`: crea la ventana en
  `resumed` (`Arc<Window>`, recurso **no-send** `PrimaryWindow`), traduce eventos a mensajes y
  estado, y corre `app.update()` desde `about_to_wait` con `ControlFlow::Poll`.
- **Compuerta de presentación:** `RedrawRequested` (en Wayland llega solo tras el frame callback
  del compositor, si se llamó `pre_present_notify`) abre `WindowState::frame_ready`. El renderer
  adquiere y presenta solo con la compuerta abierta, llama `pre_present_notify` antes de
  presentar y la cierra; el runner pide el siguiente redraw. Si un redraw pedido no llega en
  100 ms, la ventana cuenta como oculta y el runner limita a 30 Hz con `WaitUntil`.
- Al salir (`exiting`), el runner corre el schedule `Shutdown` (el renderer suelta renderer,
  superficie y dispositivo en orden) y quita la ventana, con el event loop todavía vivo.
- Input en `PreUpdate` (`ButtonInput<KeyCode>`, mouse acumulado), estilo Bevy.
- La GPU se inicializa en el primer `Render` con ventana (selección bloqueante en M0).
- Costura fina para migrar a winit 0.31 (`can_create_surfaces`/`destroy_surfaces`).

## Consecuencias

- La simulación nunca se congela por la presentación: medido con la compuerta, Vulkan (NVIDIA,
  RADV) y GL (radeonsi) siguieron actualizando con la ventana minimizada, sin presentar, y con la
  ventana visible presentan a la frecuencia del monitor. `voxel_world --minimize-test` lo
  verifica de forma automática.
- FIFO sigue siendo el modo por defecto (sin tearing, sin fps ilimitados en laptops); Mailbox es
  opcional.
- Los sistemas que tocan la ventana corren en el hilo principal (`NonSend<PrimaryWindow>`).
- Una ventana en M0 (multiventana más adelante).

## Alternativas descartadas

- Actualizar en `RedrawRequested`: se congela oculto en Wayland.
- Depender de `Occluded`: no existe en Wayland ni Windows.
- Mailbox por defecto en Vulkan: con la compuerta ya no hace falta y dibujaría sin límite.
- winit 0.31 beta: API aún cambiante.

## Revisión: límite de FPS (ADR 0013)

- El runner ya no usa `ControlFlow::Poll` por defecto: espera el deadline de `FrameLimit` (144 fps
  por defecto) con `WaitUntil` y una espera activa corta. `Poll` queda solo para el límite 0.
- Si un frame vence con la compuerta cerrada, el runner espera el frame callback hasta un cuarto
  del período (2 ms como máximo) y se engancha a él.
- El modo por defecto pasa a ser vsync apagado (`Mailbox`, si no `Immediate`, si no `Fifo`), y se
  puede cambiar en tiempo de ejecución con `PresentSettings`. Ya no hay fps ilimitados en
  laptops: los limita `FrameLimit`.

## Revisión: ritmo atado al monitor (ADR 0017)

- Con el límite en la frecuencia del monitor o más, en Wayland el frame callback decide cuándo
  corre la update (corre apenas llega, y la espera cubre un refresco salteado).
  Las updates siguen corriendo en `about_to_wait`, nunca dentro de `RedrawRequested`, y nunca
  esperan un callback sin límite de tiempo: la simulación no se congela con la ventana oculta.

## Revisión: el mundo trabaja entre frames (ADR 0022, 2026-10-08)

- La update sigue corriendo en `about_to_wait`. Si la app tiene sistemas en el schedule `Idle` y
  la update termina con espera, el runner corre pasos cortos de `Idle` (0.5 ms como máximo) en
  las iteraciones siguientes de esa espera, hasta 1 ms antes del deadline. Nunca en la
  iteración de la update: esa pone `ControlFlow::Poll` para que el redraw pedido llegue antes del
  primer paso.
- Sin sistemas en `Idle`, sin límite o en modo `Reactive`, nada cambia.
