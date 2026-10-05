# ADR 0007: renderer sin render graph

- Estado: aceptado (revisado 2026-10-05)
- Fecha: 2026-10-04

## Contexto

Un render graph genérico ayuda con muchos pases y recursos transitorios. En v1 hay pocos pases por
tier y fijos. Bevy mismo eliminó `RenderGraph` en 0.19 a favor de sistemas ordenados.

## Decisión

- Por tier, una lista fija y ordenada de pases (High: limpiar contadores, cull, opaco, cutout,
  blit; Mid: culling en CPU, opaco, cutout, blit; Low: culling en CPU, opaco y cutout con
  instancing, blit) sobre recursos explícitos (arena, tabla de chunks, view, targets).
- `VoxelRenderer` sin ECS (testeable fuera de pantalla), construido a partir de device, queue y
  tier (no de un `GpuContext`), para que el smoke test pueda crear un renderer real.
- `RenderPlugin` como capa fina con sets `Setup`, `Extract`, `Prepare`, `Draw`, `Cleanup` en el
  schedule `Render` (después de `Last`, ejecutor single-thread configurado con `edit_schedule`).
  Frontera de extract obligatoria: solo `Setup`, `Extract` y `Cleanup` tocan el mundo principal;
  `Prepare` y `Draw` leen un `ExtractedFrame` privado y el estado de GPU.
- Target de color propio + blit a la superficie (formato estable, screenshots triviales, alfa 1).
- Reverse-Z, coordenadas relativas a la cámara con resta entera en el shader. Limitación aceptada:
  en GL, wgpu remapea z a [-1, 1] sin clip control y el reverse-Z pierde su ventaja de precisión
  (~0.1 m a 384 m con near 0.1). Se mantiene near >= 0.1, un test de escena lejana lo vigila y el
  clip control en GL se propone upstream en M2.

## Consecuencias

- Código directo y fácil de depurar. Agregar Hi-Z, translúcidos, sombras o ray marching es agregar
  pases a la lista del tier que los soporte.
- Llevar el render a otro hilo (M3) es un cambio local gracias a la frontera de extract.
- Si la cantidad de pases crece mucho (M3+), se reevalúa un grafo.

## Alternativas descartadas

- Render graph tipo Bevy 0.18: complejidad sin beneficio hoy.
- Dibujar directo a la superficie: screenshots y formatos dependientes de la superficie.
- Sistemas de render que leen el mundo libremente: obligaría a reescribirlos todos en M3.
