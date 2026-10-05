# ADR 0002: tiers de capacidad y detección en tiempo de ejecución

- Estado: aceptado (revisado 2026-10-05)
- Fecha: 2026-10-04

## Contexto

El nombre del backend no dice qué se puede hacer: lavapipe (CPU) reporta todo el feature set de
alto nivel, el contexto GL de NVIDIA (3.3) dice tener compute y storage en vertex pero naga emite
GLSL 330 sin ellos, Metal no tiene multi-draw-count. Además `Instance::new` puede hacer pánico,
`request_device` puede fallar y los errores de traducción de shaders en GL son `Internal`.

La revisión del borrador encontró tres fallas: (1) un reintento podía reconfigurar una superficie
ya configurada con otro dispositivo, que en Wayland produce segfault, y el smoke test no probaba el
formato real de la superficie; (2) el LKG marcaba como malo un par por eventos que no son fallas
del driver (un pánico de Rust, cerrar la app, un error transitorio, flags de depuración); (3) el
pedido de límites podía exigir más de lo que la clasificación había comprobado.

## Decisión

- Tres tiers de ruta de render: **High** (culling en GPU + `multi_draw_indexed_indirect_count`),
  **Mid** (vertex pulling + culling en CPU + draws directos), **Low** (quads como atributo de
  instancia + immediates, sin storage buffers). "Software" es una clase de dispositivo, no un tier:
  va última y con tope Mid.
- Clasificación por features, downlevel flags, límites y versión GL. Los umbrales son constantes
  compartidas (`floors`) entre `classify` y `tier_request`, y `tier_request` nunca pide más que el
  adaptador (`Limits::default().or_worse_values_from(adapter)` más los límites que el motor
  dimensiona desde el dispositivo).
- Selección: una instancia, filtros (incluido un formato de superficie de 8 bits aceptable por
  adaptador), candidatos (adaptador, tier) ordenados por puntaje, `request_device` con features
  exactos, `on_uncaptured_error` y device-lost instalados, smoke test con los pipelines reales del
  renderer **y el formato exacto de la superficie** dentro de scopes Validation + Internal +
  OutOfMemory, chequeo de salud del dispositivo después del test, y fallback al siguiente
  candidato. Nivel del smoke `Auto`: dibujo con lectura de píxel en GL y en software.
- **Invariante de superficie:** una `Surface` configurada nunca se configura con otro dispositivo.
  Cada selección crea una superficie nueva; el renderer se construye antes de configurarla; si
  algo falla después de elegir, se excluye el par y se re-selecciona en el siguiente frame con una
  superficie nueva.
- Overrides: CLI > `BOXEL_*` > `WGPU_*` > archivo > automático.
- **LKG solo para crashes:** `pending` antes de cada intento; `good` tras el primer frame; se
  restaura el estado previo en salida limpia, pánico de Rust o fallo dentro del proceso; un
  `pending` ajeno de más de 30 s suma un strike (`suspect`, luego `bad` con dos); clave con huella
  del build; solo lectura con flags de depuración; apagado por defecto en builds de desarrollo.
- Dispositivo perdido: superficie nueva siempre; primer fallo reintenta, segundo excluye el par y
  suma un strike (no si lo causó `DebugDestroyDevice`).
- `panic = "abort"`: la instancia se crea primero sin GL y GL se intenta en una segunda instancia
  solo si hace falta.

## Consecuencias

- Arranque robusto y explicable: `gpu_info` y el log muestran cada decisión.
- Crear dispositivos y pipelines de prueba cuesta tiempo de arranque (decenas de ms por intento;
  unos ms más en GL por la lectura de píxel).
- Un fallo que no mata el proceso nunca deja marcas persistentes: se re-detecta en cada arranque.
- La política vive en un solo lugar (`boxel_gfx`), testeable con capacidades sintéticas.

## Alternativas descartadas

- Elegir por backend (`Vulkan = alto, GL = bajo`): falla con lavapipe, NVIDIA GL y Mesa GL 4.6.
- `request_adapter` de wgpu: no filtra por tier, no hace smoke tests, elige la iGPU con `None`.
- `wgpu::util::initialize_adapter_from_env`: hace pánico si el nombre no coincide.
- Una instancia por backend para sondear: Vulkan solo ya cuesta ~1 s si despierta la dGPU.
- Reintentar en el mismo frame reutilizando la superficie: es exactamente el caso del segfault.
- Guardar en el LKG todo fallo: castiga pares sanos por errores de la app o transitorios.
