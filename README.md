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
| `just test-window` | Precisión del límite de FPS con una ventana real (necesita pantalla) |
| `just test-lavapipe` / `just test-llvmpipe` | Tests de GPU por CPU (Vulkan / GL) |
| `just bench` | Benchmarks |
| `just bench-fly`, `bench-static`, `bench-spin`, `bench-dive`, `bench-all` | Mide los tirones del frame mientras carga el mundo ([docs/benchmarks.md](docs/benchmarks.md)) |
| `just gpu-info` | Informe de GPUs y qué elegiría boxel |
| `just run <ejemplo> <opciones>` | Corre `voxel_world` o `headless_world` en release |

## Ejemplos

**voxel_world**: ventana con terreno generado.

```bash
cargo run --release --example voxel_world
just run voxel_world --tier mid
```

Controles: clic izquierdo captura el mouse, Escape lo suelta. WASD para moverse, Espacio sube,
Shift izquierdo baja, Ctrl izquierdo acelera. F3 muestra el overlay de debug, F5 cambia el límite
de FPS (30, 60, 144, 240, sin límite) y F6 prende o apaga el vsync.

Opciones útiles: `--fps-limit N` (0 = sin límite), `--vsync on|off`, `--seed N`,
`--view-distance N` (en chunks), `--vertical-radius N` (chunks hacia arriba y hacia abajo, 8 por
defecto), `--size 1280x720`, `--camera x,y,z,yaw,pitch` (ángulos en grados), `--tier high|mid|low`,
`--help` (lista todo).
`--world overworld` muestra el mundo con cuevas (piedra infinita hacia abajo) en vez del bosque
de siempre; `--caves mc|closed|off` elige las cuevas y `--no-trees` saca los árboles.
Para pruebas automáticas usa siempre `--frames N --timeout SEG`: la ventana se cierra sola.
`--wait-chunks` empieza a contar frames cuando el terreno terminó de cargar y
`--screenshot shot.png` guarda una captura en el frame N. `--duration SEG` corre el juego normal
SEG segundos; al salir imprime updates y frames mostrados por segundo y el uso de CPU.

**headless_world**: sin ventana ni GPU. Genera y mallea el mundo alrededor de un punto e imprime
cuántos chunks y quads salieron y cuánto tardó. Sirve en CI y para medir la carga.

```bash
cargo run --release --example headless_world
just run headless_world --view-distance 32 --hz 144 --speed 256
```

Opciones: `--view-distance N`, `--vertical-radius N`, `--viewer-y Y` (altura del punto, 80 por
defecto; -1000 queda en las cuevas del overworld), `--hz N` (frames por segundo del bucle),
`--idle S` (se queda quieto S segundos después de cargar y mide los frames), `--speed V` (camina a
V vóxeles/s y mide los frames), `--jump N` (salta N chunks y mide hasta que la zona nueva está
lista), `--fixed-height` (carga como M0, con el alto fijo del generador; no con el overworld),
`--world flat|terrain|forest|overworld` (`terrain` por defecto), `--caves mc|closed|off` (solo
con el overworld), `--no-trees`, `--help`.

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
| Presentación | `--present vsync`, `novsync`, `mailbox` o `immediate` | `BOXEL_PRESENT` | | `present_mode` |
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

## Límite de FPS y vsync

Por defecto el motor hace como mucho 144 frames por segundo y el vsync está apagado. Los dos se
cambian mientras corre el juego, por ejemplo desde un menú de opciones:

```rust
use boxel::prelude::*;

fn opciones(mut limite: ResMut<FrameLimit>, mut vsync: ResMut<PresentSettings>) {
    limite.max_fps = 60; // 0 = sin límite
    vsync.set_vsync(true);
}
```

El cambio vale desde el frame siguiente. Con vsync nunca se muestran más frames que los Hz del
monitor. Detalles en [ADR 0013](docs/adr/0013-limite-de-fps-y-vsync-en-tiempo-de-ejecucion.md).

## Documentación

- [docs/architecture.md](docs/architecture.md): cómo está hecho y por qué.
- [docs/benchmarks.md](docs/benchmarks.md): cómo medir los tirones y comparar corridas.
- [docs/adr/](docs/adr/): una decisión por archivo.

## Licencia

MIT o Apache-2.0, a elección. Ver `LICENSE-MIT`, `LICENSE-APACHE` y `NOTICE`.
