# ADR 0001: wgpu 30 como único RHI, detrás de una capa propia delgada

- Estado: aceptado (revisado 2026-10-05)
- Fecha: 2026-10-04

## Contexto

El requisito es detectar al arrancar qué soporta el equipo (Vulkan, GL, DX12, Metal) y elegir sin
perder optimización. Opciones: escribir backends propios (ash, glow) o usar wgpu. Mediciones en la
máquina de desarrollo: un compute DDA idéntico tarda lo mismo con wgpu y con ash (55.0 a 55.5 ms
contra 54.6 a 57.1 ms). En CPU, wgpu cuesta más por comando (10 a 17x en el peor caso, rebinding
por chunk), pero un multi-draw indirecto de 10k comandos cuesta 36 µs contra 16 µs. wgpu ya
detecta backends en tiempo de ejecución con una sola instancia.

## Decisión

- wgpu `=30.0.1` es el único RHI. Sin backends crudos de Vulkan ni GL.
- `boxel_gfx` es una capa delgada de política (preferencias, tiers, selección, smoke tests, LKG),
  no una abstracción de RHI. Re-exporta `wgpu`.
- Escape hatch opcional (feature `hal`): `wgpu::hal` (as_hal, open_with_callback, SPIR-V
  passthrough) solo para Vulkan, cada uso con ADR propio. El módulo solo existe en Windows, Linux,
  Android y FreeBSD: en Apple wgpu-hal no compila su backend Vulkan sin `vulkan-portability`
  (verificado en el `build.rs` de wgpu y el manifiesto de wgpu-core), y sin esa restricción el
  job de clippy de macOS con `--all-features` fallaba.
- El renderer se diseña GPU-driven para que el costo por comando de wgpu no importe.
- DX12 usa `Dx12Compiler::Auto`: DXC si `dxcompiler.dll` está en el PATH, si no FXC (lento y sin
  mantenimiento). M0 no activa `static-dxc` (trae un DXC precompilado vía `mach-dxcompiler-rs` que
  no se puede verificar sin Windows); la CI de Windows mide el tiempo del smoke test y M1 decide
  entre `static-dxc` y distribuir la DLL.

## Consecuencias

- Una sola base de código de render para todos los SO; DX12 y Metal sin trabajo extra, con sus
  shaders validados por naga (MSL y HLSL) en cada `cargo test`.
- Pérdida realista: 0 a 5 % de CPU del frame, cerca de 0 % de GPU.
- Sin async compute, sparse, BDA ni control fino de barreras (accesibles vía hal si hiciera falta).
- Migración trimestral de API (v31 el 2026-10-09): se absorbe en `boxel_gfx` y `boxel_render`.

## Alternativas descartadas

- Backends propios Vulkan + GL: 3 a 6 personas-mes cada uno más QA de drivers; se pierden DX12 y Metal.
- Abstracción de RHI propia encima de wgpu: costo sin beneficio mientras wgpu sea el único backend.
- blade u otros: menor cobertura de plataformas y comunidad.
