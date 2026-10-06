# ADR 0011: tier Low con un solo uniform block y el origen del chunk por immediates

- Estado: aceptado (enmendado 2026-10-06 por ADR 0015: un segundo atributo de instancia)
- Fecha: 2026-10-04

## Contexto

Medido con wgpu 30.0.1 en el contexto GL 3.3 de NVIDIA: un pipeline con dos uniform blocks (view y
chunk con offset dinámico) lee los datos de la view en ambos y no dibuja nada. En wgpu-hal 30.0.1,
`SHADER_BINDING_LAYOUT` se activa cuando el contexto "soporta compute" (por extensiones, mal
detectado en 3.3), así que wgpu omite el remapeo `glUniformBlockBinding`; pero naga en GLSL 330 no
puede escribir `binding = N` y todos los bloques quedan en el binding 0. El fallo es silencioso:
ningún error, solo una imagen mal dibujada.

## Decisión

- Pipelines del tier Low (opaco y cutout): exactamente **un** uniform block (view) y **un** par
  textura + sampler.
- Datos por draw del tier Low (origen del chunk, 16 bytes) por **immediates** (`var<immediate>`,
  `set_immediates`), que wgpu GL implementa con `glUniform*` por nombre, sin bindings.
- Low requiere `Features::IMMEDIATES` (disponible en GL, Vulkan, DX12 y Metal).
- Como el fallo es silencioso, el smoke test en GL dibuja y lee un píxel (nivel `Auto` = `Draw`).
- Reportar el problema upstream (relacionado con la regresión gfx-rs/wgpu#9169).

## Consecuencias

- Low funciona en NVIDIA GL 3.3 (validado: misma imagen que Vulkan).
- `set_immediates` cuesta ~2x un draw directo en CPU: aceptable para el tier de compatibilidad.
- Cualquier recurso nuevo en Low debe respetar la regla (test de layout en `boxel_shaders`).

## Alternativas descartadas

- Uniform con offset dinámico: roto en NVIDIA GL 3.3.
- Repetir el origen del chunk por quad: duplica el formato de quad o 50 % más memoria.
- Excluir GL de NVIDIA: pierde el fallback en equipos sin Vulkan funcional.

## Enmienda (2026-10-06, ADR 0015)

Se aplica en la fase P4 de la luz.

- Low recibe la paleta de luz expandida por quad en un **segundo vertex buffer por instancia**
  (`Uint32x2` en `@location(1)`, stride 8), paralelo al arena de quads con los mismos offsets. Es
  un atributo de vértice, no un binding: la regla de un solo uniform block y un solo par textura y
  sampler no cambia.
- Una segunda textura no serviría: wgpu-hal 30.0.1 activa `SHADER_BINDING_LAYOUT` cuando el
  contexto "soporta compute" (`gles/adapter.rs:644`) y después omite el remapeo con
  `glUniform1i` (`gles/device.rs:544`), así que caería en la unidad 0. El segundo buffer sí
  funciona: sin layouts, GL reporta 16 vertex buffers (`gles/adapter.rs:800-806`) y la emulación
  de `first_instance` vuelve a enlazar cada buffer por instancia (`gles/command.rs:134-160`).
- `boxel_gfx::tier::floors::VERTEX_BUFFERS` pasa de 1 a 2. Low paga 8 bytes más por quad (16 en
  total).
