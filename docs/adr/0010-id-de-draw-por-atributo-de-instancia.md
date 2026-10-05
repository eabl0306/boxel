# ADR 0010: el dato por draw llega por atributo de instancia; VALIDATION_INDIRECT_CALL apagado

- Estado: aceptado (revisado 2026-10-05)
- Fecha: 2026-10-04

## Contexto

El vertex shader necesita saber qué chunk y qué rango de quads dibuja. Los builtins no son
portables: `draw_index` es solo Vulkan; en DX12 `SV_InstanceID`/`SV_VertexID` no incluyen
`first_instance`/`base_vertex` en la ruta `*_count` ni sin la validación indirecta; en GL 3.3 y
GLES no hay base instance. `VALIDATION_INDIRECT_CALL` es por instancia, rompe la creación del
dispositivo en NVIDIA GL y cuesta CPU por sub-draw. Además, wgpu-hal pasa `max_count` a
`vkCmdDrawIndexedIndirectCount` sin compararlo con `maxDrawIndirectCount` (mínimo 65535 en Vulkan).

## Decisión

- High y Mid: `DrawRef { chunk_slot, quad_start }` en un buffer leído como **atributo de vértice de
  paso por instancia** (`@location(0)`); cada draw usa `instance_count = 1` y
  `first_instance = índice del DrawRef`. `base_vertex = 0`, `first_vertex = 0`, `first_index = 0`
  siempre (en DX12 naga suma `first_vertex` a `SV_VertexID`; un test de naga vigila esa
  traducción).
- Low: el quad es el atributo de instancia; el origen del chunk va por immediates.
- Prohibidos en WGSL: `instance_index`, `draw_index`, `num_workgroups`, `base_instance`,
  `base_vertex` (test con naga).
- boxel nunca activa `VALIDATION_INDIRECT_CALL` (solo con `WGPU_VALIDATION_INDIRECT_CALL=1`). El
  shader de culling garantiza argumentos válidos y High exige `INDIRECT_FIRST_INSTANCE`.
- Cada lista indirecta y cada llamada `*_count` tienen como máximo 65 535 draws; el shader nunca
  escribe más allá de la capacidad de su lista.

## Consecuencias

- Mismo shader y mismo resultado en Vulkan, DX12, Metal y GL (validado: imágenes idénticas entre
  High, Mid y Low en 6 configuraciones).
- Un buffer de instancia extra (8 bytes por draw).
- Sin red de seguridad de wgpu para argumentos indirectos: responsabilidad del shader de culling.
- Con distancias de vista muy grandes (más de 65 535 buckets visibles) High omite algunos
  buckets sin orden (no los más lejanos) con un aviso; M3 segmenta las listas.

## Alternativas descartadas

- `@builtin(instance_index)` con `first_instance` = slot: roto en DX12 count path y GL sin base instance.
- `@builtin(draw_index)`: solo Vulkan.
- `base_vertex = 4 * first_quad`: `vertex_index` no lo incluye en DX12 count path.
- Uniform con offset dinámico por draw: 4 a 6x más caro por draw y roto en NVIDIA GL 3.3 (ADR 0011).
- Leer `maxDrawIndirectCount` vía `hal`: solo Vulkan y detrás de una feature opcional; el tope fijo
  es más simple y suficiente.
