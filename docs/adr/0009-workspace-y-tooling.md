# ADR 0009: convenciones de workspace y tooling

- Estado: aceptado (revisado 2026-10-05)
- Fecha: 2026-10-04

## Contexto

Proyecto con 15 crates propios, un ECS vendorizado y varios agentes implementando en paralelo. El
`/tmp` de la máquina es un tmpfs de 12 GB con cuota: los target dirs deben ir a disco y los
insumos de diseño no pueden vivir solo ahí.

## Decisión

- Edición 2024, `resolver = "3"`, toolchain fijado 1.98.1, MSRV 1.97 (`rust-version`).
- Paquete raíz `boxel` = fachada + workspace (`crates/*`); `vendor/` excluido y con su propio
  workspace y lockfile versionado.
- `[workspace.dependencies]` para todo; pins exactos en lo que rompe seguido (`wgpu =30.0.1`,
  `naga =30.0.1`, `winit =0.30.13`, `glam =0.34.0`, `binary-greedy-meshing =0.5.2`).
- `[workspace.lints]`: `missing_docs`, `unsafe_code = deny` (opt-in con `#[expect]`), lints de
  clippy de Bevy; `rust_2018_idioms` apagado (ruido en parámetros de sistemas).
- Features de la fachada: `debug` (nombres en diagnósticos del ECS) activa por defecto; los crates
  del motor la activan solo para sus tests (dev-dependency).
- Sin config de linker en Linux (rust-lld por defecto). Perfiles: dev opt 1 con deps opt 3,
  release thin LTO, `profiling`, `dist`.
- `CARGO_BUILD_WARNINGS=deny` en CI y en `just lint`/`just doc`. cargo-deny 0.20 (licencias
  permisivas, nombres `bevy*` prohibidos, una sola versión de wgpu/naga/glam/winit) más
  `tools/no_bevy.py` y `tools/vendor_lock_sync.py`. typos (código; los docs en español se
  excluyen; la receta avisa si falta y `BOXEL_SKIP_TYPOS=1` lo saltea de forma explícita).
  justfile como interfaz única. GitHub Actions con acciones fijadas por SHA, tests de GPU con
  lavapipe/llvmpipe en un hilo, Miri en un workflow propio (PRs del fork y semanal, nightly fijado).
- Logs con `tracing`; Tracy opcional; criterion para benchmarks; naga valida todos los WGSL en
  todos los backends de escritorio en `cargo test`.
- Documentos técnicos en español; código, identificadores y rustdoc en inglés.
- Implementación del M0 en paralelo: copia privada del repositorio por paquete, target dir semilla
  compartido, compuerta de propiedad (`.scaffold/`, borrada al cerrar M0).

## Consecuencias

- Reproducible y estricto desde el día uno; el costo es mantener pins y SHAs (Dependabot para Actions).

## Alternativas descartadas

- `RUSTFLAGS=-Dwarnings`: invalida la caché; `build.warnings` (Cargo 1.97) no.
- mold/cranelift por defecto: opcionales por desarrollador (`config.local.toml`).
- Varios agentes en un solo directorio compartido: un error de compilación a medio editar en un
  crate bloquea a todos los que dependen de él.
