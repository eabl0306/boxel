# boxel

Motor de vóxeles en Rust, al estilo de Bevy, sobre wgpu. Al arrancar elige solo la GPU, el
backend (Vulkan, GL, DX12, Metal) y el tier de render (High, Mid o Low) que la máquina soporta.
Si algo falla, prueba el siguiente.

Estado: milestone M0, en desarrollo. Solo escritorio (Linux, Windows, macOS).

## Requisitos

- Rust 1.98.1. `rustup` lo instala solo gracias a `rust-toolchain.toml`. Versión mínima: 1.97.
- [just](https://github.com/casey/just) 1.56 o más nuevo.
- Una GPU con Vulkan, DX12, Metal u OpenGL 3.3 o más (el tier Mid en GL pide 4.3). Sin GPU
  también funciona con lavapipe o llvmpipe (Mesa, por CPU).
- En Linux: Wayland o X11.
- Para `just lint`, `just deny` y `just ci`: `python3`. Para `just deny` y `just ci`, además:
  `cargo-deny`. Para `just ci`, también `typos-cli` (sin typos: `BOXEL_SKIP_TYPOS=1`).

## Comandos

| Comando | Qué hace |
|---|---|
| `just` | Lista todas las recetas |
| `just test` | Tests de la workspace (los de GPU se saltan si no hay adaptador) |
| `just lint` | Clippy con los warnings como error |
| `just fmt` | Formatea el código |
| `just doc` | Genera la documentación |
| `just ci` | Lo principal de la CI, en local (MSRV y los tests de GPU por CPU van aparte) |
| `just test-gpu` | Tests de GPU en todos los adaptadores reales |
| `just test-lavapipe` / `just test-llvmpipe` | Tests de GPU por CPU (Vulkan / GL) |
| `just bench` | Benchmarks |
| `just gpu-info` | Informe de GPUs y qué elegiría boxel |
| `just run <ejemplo> <opciones>` | Corre `voxel_world` o `headless_world` en release |

## Ejemplos

**voxel_world**: ventana con terreno generado.

```bash
cargo run --release --example voxel_world
just run voxel_world --tier mid
```

Controles: clic izquierdo captura el mouse, Escape lo suelta. WASD para moverse, Espacio sube,
Shift izquierdo baja, Ctrl izquierdo acelera.

Opciones útiles: `--seed N`, `--view-distance N` (en chunks), `--size 1280x720`,
`--camera x,y,z,yaw,pitch` (ángulos en grados), `--tier high|mid|low`, `--help` (lista todo).
Para pruebas automáticas usa siempre `--frames N --timeout SEG`: la ventana se cierra sola.
`--wait-chunks` empieza a contar frames cuando el terreno terminó de cargar y
`--screenshot shot.png` guarda una captura en el frame N.

**headless_world**: sin ventana ni GPU. Genera y mallea el mundo alrededor de un punto e imprime
cuántos chunks y quads salieron. Sirve en CI.

```bash
cargo run --release --example headless_world
```

**gpu_info**: lista los adaptadores, cómo los clasifica boxel y qué elige.

```bash
cargo run -p boxel_gfx --example gpu_info -- --all-tiers --smoke draw
```

**window_smoke**: abre una ventana vacía y la cierra sola.

```bash
cargo run -p boxel_window --example window_smoke
```

## Elegir la GPU a mano

Por defecto todo es automático. Para forzar algo, usa la opción de línea de comandos, una
variable de entorno o un archivo TOML. Gana el primero que esté definido, en este orden:
CLI, `BOXEL_*`, `WGPU_*`, archivo.

| Qué | CLI | `BOXEL_*` | `WGPU_*` | Archivo `[gpu]` |
|---|---|---|---|---|
| Backends | `--backend vulkan,gl` | `BOXEL_BACKEND` | `WGPU_BACKEND` | `backends` |
| Adaptador (parte del nombre) | `--adapter nvidia` | `BOXEL_ADAPTER` | `WGPU_ADAPTER_NAME` | `adapter` |
| Energía | `--power high` o `low` | `BOXEL_POWER` | `WGPU_POWER_PREF` | `power` |
| Tier máximo | `--tier high`, `mid` o `low` | `BOXEL_TIER` | | `max_tier` |
| Solo CPU | `--software` | `BOXEL_SOFTWARE=1` | | `software` |
| Validación | `--validation` | `BOXEL_GPU_VALIDATION=1` | | `validation` |
| Memoria de crashes (LKG) | `--lkg` / `--no-lkg` | `BOXEL_LKG=1` o `0` | | `use_lkg` |
| Presentación | `--present vsync`, `mailbox` o `immediate` | `BOXEL_PRESENT` | | `present_mode` |
| Archivo | `--gpu-config ruta.toml` | `BOXEL_GPU_CONFIG` | | |

`--tier` es un tope, no una orden: boxel nunca sube de ahí, pero puede bajar. En un portátil con
dos GPUs, `--power low` elige la integrada.

Ejemplo de archivo:

```toml
[gpu]
backends = "vulkan"
power = "low"
max_tier = "mid"
```

## Documentación

- [docs/architecture.md](docs/architecture.md): cómo está hecho y por qué.
- [docs/adr/](docs/adr/): una decisión por archivo.

## Licencia

MIT o Apache-2.0, a elección. Ver `LICENSE-MIT`, `LICENSE-APACHE` y `NOTICE`.
