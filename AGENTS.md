# Guía para agentes (Cursor, Claude y otros)

boxel es un motor vóxel en Rust, estilo Bevy: chunks de 32³ infinitos en todas las direcciones,
coordenadas `i64`, luz de color tipo Minecraft, físicas propias y render con wgpu en tres tiers
(High, Mid y Low). Este repositorio es **solo el motor**. El juego que lo usa (un clon de Minecraft)
va en `game/`, que git ignora. El contenido de los ejemplos (bloques, texturas, fluidos, recetas de
packs y generadores) vive en `crates/boxel_demo` (ADR 0029).

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
- **El motor no nombra contenido.** Bloques, texturas, formas, reglas, fluidos y generadores
  concretos van en `crates/boxel_demo`. Los crates del motor usan nombres `game:` en la
  documentación y `test:` en los tests (`boxel_voxel::testing`). `tools/no_content.py` lo revisa
  y no tiene lista de excepciones.
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
python3 tools/no_content.py
```

`cargo fmt --check` va siempre sin `--all`, porque `--all` reformatearía `vendor/`. Si tocaste algo
de GPU, corre también los tests de GPU:
`BOXEL_REQUIRE_GPU=1 tools/quiet.sh cargo test --workspace --locked --test '*gpu*' -- --test-threads=1`.

`tools/no_content.py` falla si un crate del motor nombra contenido concreto (un nombre `boxel:`
que no está reservado, o un identificador del juego de ejemplo) o depende de `boxel_demo` fuera de
los tests.

## Cómo medir

Medir llevaba más tiempo que las tareas, así que hay dos niveles.

- **En cada tarea, solo el nivel 1.** Primero los números que no tienen ruido: quads y chunks con
  `headless_world`, y un bench de criterion solo del camino caliente que tocaste (una corrida por
  lado). Después `just perf-smoke CASO...` con uno o dos casos que toque tu tarea
  (`python3 tools/perf/perf.py cases` los lista; sin casos mide superficie y bosque). Las opciones
  de stress van al final, tras `--`. Tarda de 5 a 10 minutos. Pega su tabla en la PR.
- **El smoke sale con 0 si pasa**, 1 si algo empeoró mucho o tu binario falló, y 2 si no pudo medir.
  Un 2 no es un "ok": repítelo con la máquina tranquila.
- **No lo envuelvas en `tools/quiet.sh`.** Ya compila con él y se niega a correr dentro.
- **No escribas tu propio harness** ni rondas de corridas intercaladas. Usa `tools/perf/`, y si le
  falta algo, agrégalo ahí con su test.
- **Nunca estudies refrescos salteados con tope en una tarea.** Salen con mucho ruido por el estado
  de energía de la NVIDIA (issue #5).
- **La ronda de control** (`just perf-check`, más de una hora) la corre solo quien integra en
  `main`, sobre `main`: cada 4 o 5 tareas integradas o antes de cerrar un hito. Una tarea nunca la
  corre. Si algo empeoró, el informe trae el `just perf-bisect` que busca el merge.

Detalle en `docs/benchmarks.md`.

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
- `docs/adr/`: decisiones 0001 a 0038.
- `docs/planes/`: planes completos de cuevas, luz y físicas.
- `docs/rendimiento/`: mediciones.
- `docs/benchmarks.md`: cómo medir. `tools/perf/` tiene el harness de las recetas `perf-*`.
- `crates/boxel_demo`: el contenido de los ejemplos (bloques, texturas, formas, reglas, agua y
  lava, recetas de packs y generadores). No es parte del motor.
- `examples/`:
  - `voxel_world`: el mundo jugable. G camina, los clics rompen y ponen bloques, F3 muestra el
    overlay.
  - `physics_sandbox`: demos de físicas.
  - `snapshot`: capturas sin ventana.
  - `headless_world`.
  - `stress`: mediciones.
