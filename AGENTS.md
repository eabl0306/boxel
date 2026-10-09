# Guía para agentes (Cursor, Claude y otros)

boxel es un motor vóxel en Rust, estilo Bevy: chunks de 32³ infinitos en todas las direcciones,
coordenadas `i64`, luz de color tipo Minecraft, físicas propias y render con wgpu en tres tiers
(High, Mid y Low). Este repositorio es **solo el motor**. El juego que lo usa (un clon de Minecraft)
va en `game/`, que git ignora.

Antes de empezar, lee la issue #1 del repo ("Contexto para retomar") y la issue que te toque en el
GitHub Project. Allí están el estado, las decisiones tomadas y los números medidos.

## Reglas que no se discuten

- **Trabaja en una rama**, nunca en `main`, y no hagas push a `main`. El dueño o Claude revisan e
  integran.
- **Compila y prueba a través de `tools/quiet.sh`** (por ejemplo `tools/quiet.sh cargo test -p
  boxel_world`). Limita los trabajos pesados y la RAM para no frenar el portátil, y se niega a
  arrancar con menos de 40 GB libres.
- **No toques sin pedirlo antes:** el ritmo de frames (`crates/boxel_window/src/runner.rs`,
  `crates/boxel_app/src/frame_limit.rs`), los shaders y el render (`crates/boxel_render`,
  `crates/boxel_shaders`, `crates/boxel_gfx`), el streaming del mundo
  (`crates/boxel_world/src/streaming.rs`, `interest.rs`), el interior de `crates/boxel_physics`, ni
  nada de rendimiento. Esas partes tienen reglas finas. Por ejemplo, el tier Low admite un solo
  uniform block, las imágenes deben salir iguales píxel a píxel en los tres tiers, y la física es
  determinista: no se usan `mul_add` ni funciones de libm.
- **No edites `vendor/`.** Es un fork del ECS de Bevy con cero cambios de librería (ADR 0003); se
  regenera con `tools/refork/refork.sh`.
- **No agregues dependencias `bevy_*`.** Para cualquier dependencia nueva, pasa `cargo deny check`.
- **Nunca subas `assets/`** (texture packs de la comunidad con copyright) ni `game/`.
- **No corras benchmarks ni `examples/stress`** mientras algo compila, porque los números salen mal.
  Los ejemplos con ventana, siempre con `--timeout`.

## Antes de cada commit

Todo debe pasar:

```bash
cargo fmt --check
tools/quiet.sh cargo lint
tools/quiet.sh cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" tools/quiet.sh cargo doc --workspace --no-deps --locked
typos
python3 tools/no_bevy.py
```

`cargo fmt --check` va siempre sin `--all`, porque `--all` reformatearía `vendor/`. Si tocaste algo
de GPU, corre también los tests de GPU:
`BOXEL_REQUIRE_GPU=1 tools/quiet.sh cargo test --workspace --locked --test '*gpu*' -- --test-threads=1`.

## Estilo

- **Código:** comentarios y mensajes de commit en inglés. Imita el estilo y la densidad de
  comentarios del código que tienes al lado. Toolchain fijado en `rust-toolchain.toml`, edición 2024.
- **Docs en `docs/`:** en español, sin guiones largos y sin emojis. Los textos para personas van
  cortos, simples y directos.
- **Tests:** cada cambio de comportamiento lleva su test. El repo usa proptest para propiedades y
  tests "lejos del origen" (coordenadas cerca de ±2^31, 2^40 y el límite del mundo, ver ADR 0014).
- **Commits:** prefijo convencional (`feat(scope):`, `fix(scope):`, `docs:`, ...) y un cuerpo que
  explique el porqué.

## Dónde está cada cosa

- `docs/architecture.md`: arquitectura, mapa de crates y roadmap.
- `docs/adr/`: decisiones 0001 a 0023.
- `docs/planes/`: planes completos de cuevas, luz y físicas.
- `docs/rendimiento/`: mediciones.
- `docs/benchmarks.md`: cómo medir.
- `examples/`:
  - `voxel_world`: el mundo jugable. G camina, los clics rompen y ponen bloques, F3 muestra el
    overlay.
  - `physics_sandbox`: demos de físicas.
  - `snapshot`: capturas sin ventana.
  - `headless_world`.
  - `stress`: mediciones.
