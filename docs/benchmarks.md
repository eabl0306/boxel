# Benchmarks de tirones (stress)

Sirve para medir los tirones del frame mientras el mundo carga chunks nuevos, antes y después de
cada optimización. El programa es `examples/stress.rs`.

## Cómo correrlo

```bash
just bench-fly                          # vuela a 32 vóxeles/s, radio 16
just bench-fly --speed 64 --radius 32
just bench-static --radius 32
just bench-spin
just bench-all --radius 16              # static, fly a 32 y a 64, spin
```

Cada corrida abre una ventana que se cierra sola (unos 25 s) y deja dos archivos en
`bench-results/` (no se suben a git):

- `<fecha>-<escenario>.md`: el resumen.
- `<fecha>-<escenario>.csv`: una fila por frame.

Opciones útiles (todas en `cargo run --release --example stress -- --help`): `--radius N`,
`--speed V`, `--duration S` (20 por defecto), `--warmup S` (2), `--seed N`, y las de GPU de
siempre: `--backend vulkan`, `--adapter nvidia`, `--tier mid`.

El stress corre siempre sin límite de FPS. `--vertical-radius N` fija cuántos chunks se cargan
arriba y abajo de la cámara (8 por defecto). `--fixed-height` carga solo las capas del generador,
como en M0: úsalo para comparar con corridas viejas, que no tienen radio vertical.

Para que los números sirvan:

- Siempre en release (las recetas ya lo hacen).
- Con la máquina quieta: nada compilando ni otro juego abierto. Las recetas compilan primero y
  después esperan hasta 60 s a que la CPU se calme (`tools/wait-idle.sh`). Si no se calma, corren
  igual pero la etiqueta de la corrida termina en `-busy`: esos números no sirven para comparar.
  El resumen también anota la carga del sistema (load average) al empezar y al terminar.
- Comparar solo corridas con la misma GPU, escenario, radio y velocidad.

## Escenarios

| Escenario | Qué hace la cámara | Qué mide |
|---|---|---|
| `static` | queda quieta | el costo fijo con el mundo ya cargado |
| `fly` | vuela recto a velocidad fija | los tirones mientras entran chunks nuevos |
| `spin` | gira 360 grados en su lugar | el costo de que cambie lo que se ve, sin chunks nuevos |

Todos esperan a que el mundo termine de cargar, corren `--warmup` segundos sin medir y después
miden `--duration` segundos.

El bucle corre sin límite de fps y dibuja en cada vuelta, aunque la ventana esté tapada. Los fps
de acá dicen cuántos frames puede hacer el motor, no cuántos muestra el monitor: solo se presenta
un frame por cada refresco del monitor (unos 144 por segundo) y el resto se dibuja sin mostrarse.

## Qué significa cada número

Un frame es una vuelta entera del bucle: mundo y render. Solo algunos llegan a la pantalla (ver
arriba).

| Número | Qué es |
|---|---|
| avg fps | frames por segundo en promedio |
| p50, p95, p99, p99.9 | el 50 %, 95 %, 99 % o 99.9 % de los frames tardó esto o menos. p99.9 es el peor de cada 1000 |
| max | el peor frame |
| 1% low | los fps del 1 % de frames más lentos. Cuanto más cerca de avg fps, más parejo |
| > 6.94 ms | frames que no entran a 144 Hz |
| > 16.7 ms | frames que no entran a 60 Hz: un tirón que se nota |
| frames con cruce de borde | frames en que la cámara pasó a otro chunk. Ahí el mundo recalcula qué chunks quiere |
| generated/s, meshes/s | chunks generados y mallas subidas a la GPU por segundo |
| jobs queued, in flight | trabajos esperando y trabajos corriendo |
| queue trend | cuánto crece la cola por segundo, mirando toda la medición. Si es claramente mayor que 0, el mundo se atrasa |
| border catch-up | después de cada cruce, cuánto tardó el mundo en terminar los chunks nuevos. Si no termina antes del próximo cruce, va atrasado. Si la medición corta el último cruce a la mitad, ese no cuenta |
| catch-up after stop | cuánto tardó en terminar todo después de que la cámara frenó |
| Worst 5 frames | los 5 peores, con lo que pasó en cada uno (cruce, chunks generados, mallas, cola) y si se presentó |
| Per second | fps, peor frame y pico de la cola, segundo a segundo |

Los chunks generados y los trabajos terminados se cuentan exactos (stress envuelve el generador
y el ejecutor de trabajos). El resto sale de `WorldStats`, de `RenderInfo` y de los mensajes
`ChunkMeshEvent`, sin tocar el motor.

## Comparar dos corridas

```bash
just bench-report bench-results/                    # una fila por corrida
just bench-report --compare antes.md despues.md
just bench-report --compare carpeta-antes/ carpeta-despues/
```

`--compare` muestra cada número, el cambio en % y si mejoró o empeoró. Si antes era 0 no hay %,
pero igual dice si empeoró (por ejemplo, tirones nuevos). Si las corridas tienen otra
configuración (radio, GPU, velocidad), lo avisa. Con dos carpetas empareja las corridas del mismo
escenario, radio, velocidad, backend, tier y GPU.

Flujo sugerido: `just bench-all` antes del cambio, mover `bench-results/` a otra carpeta,
aplicar el cambio, `just bench-all` otra vez y comparar las dos carpetas.

## El CSV

Una fila por frame: `frame_ms`, `crossed` (cruzó un borde), `generated`, `meshes`, `removed`,
`queued_jobs`, `in_flight`, los contadores de `WorldStats` y la posición de la cámara. Sirve para
graficar o para buscar qué pasó en un tirón puntual.

## Tiempo por sistema (--spans)

Para saber en qué se va el tiempo de un frame, sistema por sistema:

```bash
cargo run --release --features trace --example stress -- --scenario fly --speed 64 \
    --radius 32 --summary out.md --spans --spans-csv out-spans.csv
```

Con `--features trace` cada sistema del ECS abre un span de `tracing`, y el mundo y el render
agregan spans propios por dentro (`world.positions`, `world.stats_walk`, `world.drain`,
`world.dispatch`, `render.upsert`, `render.submit`...). `world.step create`, `unload`, `update`,
etc. reparten el tiempo de `world.positions` por tipo de paso. `--spans` suma el tiempo de cada
uno en cada frame y agrega al resumen la sección "Time per system":

- media, p99 y máximo por sistema en tres grupos de frames: cruce de borde, puesta al día (los
  frames después del cruce hasta que el mundo termina) y el resto;
- el total por cruce de borde, sumando el cruce y su puesta al día;
- los 10 peores frames de cruce, con lo que más tardó en cada uno.

`--spans-csv` deja lo mismo frame a frame. Para comparar dos versiones, las dos tienen que estar
compiladas con `--features trace`, porque los spans también cuestan tiempo: en radio 32, cerca de
un 1 % de fps, y unos 50 ns más por posición en `world.positions`.

Detalles:

- Sin `--features trace` el flag da error y los spans no existen en el binario. Con la feature
  pero sin `--spans`, los spans igual cuestan (los recibe el log), solo que nadie los suma.
- Los sistemas que corren en paralelo suman más que el frame. Las filas `schedule` son tiempo
  real en el hilo principal, y `outside Main` es lo que pasa fuera de `App::update` (winit).
- `RUST_LOG` tiene que dejar pasar spans de nivel `info`. El filtro por defecto lo hace.

## Para juegos

Cualquier juego con `FrameStatsPlugin` (está en `DefaultPlugins`) tiene el recurso
`FrameTimeHistory`: los últimos 4096 frames, con `summary()` para p50, p99, p99.9, máximo y
1% low. Sirve para mostrarlo en pantalla o para avisar de tirones.

## Notas

- Para compilar o correr tests en segundo plano sin frenar el escritorio está `tools/quiet.sh`
  (ejemplo: `tools/quiet.sh cargo test --workspace`). Deja como mucho 2 trabajos pesados a la vez,
  con prioridad baja y un tope de RAM, y no arranca si quedan menos de 40 GB de disco.
- La primera medición está en [rendimiento/baseline-2026-10.md](rendimiento/baseline-2026-10.md).
