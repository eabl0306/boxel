# Benchmarks de tirones (stress)

Sirve para medir los tirones del frame mientras el mundo carga chunks nuevos, antes y después de
cada optimización. El programa es `examples/stress.rs`.

## Qué correr y cuándo

Hay dos niveles, porque las campañas largas de stress llevaban más tiempo que las tareas. Las
tres recetas usan el harness de `tools/perf/`. No escribas otro.

### Nivel 1: en cada tarea (5 a 10 minutos)

1. Primero los números que no tienen ruido: quads y chunks con `headless_world`, y si tocaste un
   camino caliente, su bench de criterion (`just bench mesh`, por ejemplo). Una corrida por lado.
   Los draws no salen de `headless_world` (no tiene render): salen del resumen de `stress`, en la
   línea final del mundo de una corrida de `just bench-static` por lado.
2. Después el smoke, con uno o dos casos que toque tu tarea (`python3 tools/perf/perf.py cases`
   los lista):

```bash
just perf-smoke                        # superficie y bosque en la RTX, radio 16, sin tope
just perf-smoke dive cave-path         # los casos que toca la tarea
just perf-smoke 780m/forest/144        # un caso es gpu/escenario/rN/tope, en cualquier orden
just perf-smoke forest -- --bodies 10000   # opciones de stress, iguales en los dos lados
just perf-smoke --base main            # otra base (por defecto, el merge base con main)
just perf-smoke --base HEAD "--new-extra='--fog valley'" 780m/surface   # solo el lado nuevo
```

`--new-extra` le pasa opciones de stress solo al lado nuevo, después de las que van tras `--`.
Con `just` va entre comillas dobles y el valor entre simples, como arriba: `just` vuelve a partir
los argumentos por los espacios.
Sirve para medir el costo de una opción con el mismo binario: con `--base HEAD` los dos lados son
el mismo código y solo cambia la opción. La tabla y los nombres de los casos la muestran como
`[new: ...]`.

Compila la rama tal como está y toma el binario de la base de `bench-results/bins/<sha>/stress`.
Si no está, lo compila en un worktree aparte, en `target-perf/` o en `PERF_TARGET_DIR`. Cada
target dir es de un solo árbol: si otro árbol ya compiló ahí con el harness, se niega, porque
cargo puede tomar el código de un árbol por el del otro. Si la rama cambia mientras compila (un
commit, una edición), no mide.

Las recetas `perf-*` ya compilan con `tools/quiet.sh`. No las envuelvas en él: se niegan a correr
dentro de un slot, que además frenaría sus compilaciones.

Corre una vez 10 s por lado cada caso y solo falla con un empeoramiento grosero:

- fps por debajo del 85 % de la base,
- p99 de más de 1.5 veces la base,
- frames de más de 6.94 ms donde la base no tenía.

Si un caso falla, repite el par una vez y solo cuenta si vuelve a fallar. Con tope 144 la base
siempre tiene unos 1500 frames de más de 6.94 ms (ver
[rendimiento/ritmo-2026-10.md](rendimiento/ritmo-2026-10.md)), así que ahí el smoke solo mira fps
y p99. En la RTX con tope, el p99 solo cuenta si las dos corridas cayeron en el mismo modo de
refrescos salteados (ver el nivel 2). Los topes van de 50 para arriba: más bajos, no se distinguen
del límite de 30 fps del compositor.

Al final imprime una tabla para pegar en la PR, solo con números de corridas que cuentan. Sale
con:

- 0: todo medido y sin empeoramiento grosero.
- 1: algo empeoró mucho, o el binario de la rama falló (o perdió un escenario) donde la base
  corrió.
- 2: no pudo medir algo. Por ejemplo, falló la base o fallaron los dos lados (sin GPU), una
  corrida siguió con ruido tras 4 intentos, la máquina no quedó tranquila en 30 minutos
  (`PERF_MAX_WAIT`) o la base no tiene el escenario. Un 2 no es un "ok": repítelo con la máquina
  tranquila.

En una tarea no se estudian refrescos salteados con tope. Salen con mucho ruido por el estado de
energía de la NVIDIA (issue #5).

### Nivel 2: la ronda de control

La corre solo quien integra en `main` (el dueño, o Claude cuando integra), sobre un commit de
`main`, cada 4 o 5 tareas integradas o antes de cerrar un hito. Una tarea nunca la corre.

```bash
just perf-check --dry-run              # solo el plan y cuánto va a tardar
just perf-check                        # más de una hora
```

Corre una matriz fija (`python3 tools/perf/perf.py cases` la lista): RTX y 780M; superficie,
cueva abierta (ruta y quieta), bosque y bajada; con tope 144 y sin tope; radio 16, más una cueva
y una superficie en radio 32 en la RTX. Hace 3 corridas intercaladas de 20 s por caso contra el
binario guardado de la ronda anterior (`bench-results/control/last`, o el tag `perf-control-N`
más alto si ese archivo se perdió), sin recompilarlo, y mira el estado de la NVIDIA (solo
lectura). Escribe `docs/rendimiento/control-AAAA-MM-DD.md`. La primera ronda no tiene contra qué
comparar y solo guarda los números. Los casos de cueva corren cuando el binario tiene `--cave`.

Solo una ronda completa, en la que contaron todas las corridas, pasa a ser la base de la próxima
y recibe el tag local `perf-control-N`, que sube el dueño. Si falta algo (la RTX no aparece, una
corrida siguió con ruido), el informe dice "Ronda incompleta" y la base no se mueve;
`--accept-partial` la acepta igual. Sale con 0 si todo está bien, 1 si algo empeoró o el binario
nuevo falló o perdió un escenario, y 2 si la ronda quedó incompleta. Fuera de `main` se niega;
`--off-main` mide igual, deja el informe en `bench-results/` y no toca el tag ni la base.

Con tope, cada corrida de la RTX cae al azar en un modo con muchos refrescos salteados (unos 5
por segundo) o en uno con pocos (cerca de 1), por el estado de energía de la NVIDIA (issue #5).
Una corrida está en el modo alto si saltea refrescos en más del 1 % de sus frames. Ahí su p99 es
un frame doble (13.9 ms) y sus frames lentos siguen a los salteos. Por eso, en esos casos, p99 y
frames lentos se comparan solo entre corridas del mismo modo, en un modo con 2 corridas o más de
cada lado. El informe muestra cuántas cayeron en el modo alto y no marca eso: con 3 corridas por
lado, ni 0 de 3 contra 3 de 3 se distingue del azar (p = 0.10). Antes de esta regla, el bosque con
tope se marcó por error justo por eso en la ronda 1
([rendimiento/control-2026-10-09.md](rendimiento/control-2026-10-09.md)). Si ningún modo junta 2 y
2, las notas dicen que el caso quedó sin juzgar en esas dos métricas. La tabla sigue con la media
de todas las corridas, así que un salto se ve aunque no se marque.

Si algo empeoró, el informe trae el comando de `perf-bisect` que busca el merge. Prueba por
mitades los commits de primer padre (los merges) entre las dos refs. Por defecto usa el smoke del
caso; con `--metric` usa la regla de la ronda para esa métrica (3 corridas de 20 s por lado), que
encuentra cambios que el smoke no ve, como la puesta al día o el p50:

```bash
just perf-bisect rtx/surface/r16/144 perf-control-3 main
just perf-bisect rtx/surface/r16/uncapped perf-control-3 main --metric catch_up
```

`--fps-min` y `--p99-max` solo cambian la regla del smoke, así que el bisect con `--metric` los
rechaza. El bisect muestra los números de cada corrida.

Sin tope, a veces la RTX pasa una corrida entera, o un tramo largo, un 12 % más lenta al mismo
reloj y con menos potencia, con cualquier binario
([rendimiento/sin-tope-lento-2026-10.md](rendimiento/sin-tope-lento-2026-10.md)). Una corrida así
entre otras normales baja la media un 5 a 7 % sin que el código cambie. Como el recorrido de cada
escenario es fijo, el informe y el bisect comparan cada segundo con el de la otra versión.
Nombran las corridas que van más de un 8 % por debajo en un cuarto de sus segundos o más, y dan
la media sin ellas. En el informe, la celda de fps muestra la corrida más lenta y la más rápida
de un lado cuando se separan más de un 5 %.

En la RTX, la ronda y el bisect (y el smoke con `--nvidia`) dejan el estado de la NVIDIA en el
`.nv.csv` de cada corrida: P-state, relojes, potencia, uso, `clocks_event_reasons.active` (la
máscara de motivos por los que la GPU baja su reloj o su trabajo), temperatura y la frecuencia
media de la CPU.

### Cómo se cuida el ruido

Antes de cada corrida el harness espera a que no compile nada (rustc, cargo, linker), a que no
corra otro binario de boxel y a que `tools/wait-idle.sh` pase. Mira la máquina cada 0.5 s mientras
corre y repite, hasta 4 veces, las corridas que se cruzaron con una compilación u otro benchmark,
en las que el resto de la máquina usó más del 15 % de CPU, o en las que el compositor bajó la
ventana a 30 fps (un segundo con menos de 100 fps o de 50 frames mostrados, o con menos del 70 %
de un tope más bajo). Una corrida que sigue con ruido tras el cuarto intento no cuenta: no entra
en ningún veredicto ni en ninguna media, y sus archivos van a `discarded/`. Si la máquina no queda
tranquila en `PERF_MAX_WAIT` segundos (30 minutos), el smoke no corre los casos que faltan. Dos
harnesses nunca miden a la vez. Cada corrida queda en `bench-results/` (smoke, control o bisect)
y `just bench-report --compare` compara las carpetas `base` y `new`.

## Cómo correrlo

```bash
just bench-fly                          # vuela a 32 vóxeles/s, radio 16
just bench-fly --speed 64 --radius 32
just bench-static --radius 32
just bench-spin
just bench-dive --world overworld --speed 64   # baja en línea recta por las cuevas
just bench-all --radius 16              # static, fly a 32 y a 64, spin
```

Cada corrida abre una ventana que se cierra sola (unos 25 s) y deja dos archivos en
`bench-results/` (no se suben a git):

- `<fecha>-<escenario>.md`: el resumen.
- `<fecha>-<escenario>.csv`: una fila por frame.

Opciones útiles (todas en `cargo run --release --example stress -- --help`): `--radius N`,
`--speed V`, `--duration S` (20 por defecto), `--warmup S` (2), `--seed N`, y las de GPU de
siempre: `--backend vulkan`, `--adapter nvidia`, `--tier mid`.

El stress corre sin límite de FPS. `--fps-limit N` lo limita a N frames por segundo (0, el valor
por defecto, es sin límite), por ejemplo para ver cómo se comporta con el límite de 144 de un
juego. El resumen anota el límite y `tools/bench_report.py` no mezcla corridas con límites
distintos (las corridas viejas, sin el dato, cuentan como sin límite).

Con límite, el mundo trabaja también entre frames (ADR 0023). `--idle-work off` lo apaga, para
comparar el mismo binario con y sin eso. El resumen lo anota (`idle_work`: `on`, `off`, o
`none` en corridas viejas) y `tools/bench_report.py` solo avisa si cambia: empareja igual las
corridas, así se puede comparar `off` con `on`, o main con una rama. `headless_world` tiene la
misma opción.

El stress presenta con `--present immediate` (en Wayland sale Mailbox). `--vsync on` pide vsync
(Fifo) y `--vsync off` el modo por defecto de un juego (Mailbox, si no Immediate). El resumen anota
el vsync, el modo que quedó y la frecuencia del monitor, y `tools/bench_report.py` no mezcla
corridas con y sin vsync.

`--edits` agrega ediciones con guion durante la medición, una por segundo cerca del inicio: pone
una lámpara y la saca, cava un pozo de 24 bloques y lo tapa. El resumen suma una tabla "Edits"
con los frames que tardó cada una en publicarse (de la edición hasta que el mundo vuelve a estar
quieto, el frame de la edición incluido) y los lotes de ediciones retenidos o partidos. Sirve con
`--scenario static`. `--time HH:MM` fija la hora del mundo (12:00 por defecto); el reloj queda
quieto toda la corrida y el resumen la anota. `tools/bench_report.py` no mezcla corridas con y
sin `--edits`, compara la latencia de las ediciones cuando la hay y solo avisa si cambia la hora
(el día y la noche no cambian ninguna malla).

Cuerpos de física (ADR 0016): `--bodies N` pone N cuerpos sobre el suelo alrededor del inicio de
la cámara cuando el mundo terminó de cargar, tan repartidos como en los benches de física (10 000
en un cuadrado de 304 bloques). `--body-kind item` (por defecto) son objetos tirados que caen y se
duermen; `--body-kind mob` son caminantes que cambian de rumbo y nunca se duermen.
`--projectiles M` dispara M flechas sobre la misma zona y vuelve a disparar cada una cada 2 s,
con los disparos repartidos en esos 2 s (cerca de la mitad de M en el aire a la vez). Con
cuerpos, la app suma `PhysicsPlugin` (y su tope de 3 pasos fijos por frame); sin ellos nada
cambia. El resumen y `tools/bench_report.py` anotan los cuerpos y no mezclan corridas con cuerpos
distintos. La regla del plan: `--bodies 1000` y `--bodies 10000` (con objetos dormidos) no deben
sumar ningún frame de más de 6.94 ms contra la misma corrida sin cuerpos, y el p99 sube como mucho
0.3 ms. Conviene un `--warmup` de 3 s o más, así los objetos llegan a dormirse antes de medir.

`--vertical-radius N` fija cuántos chunks se cargan arriba y abajo de la cámara (8 por defecto).
`--fixed-height` carga solo las capas del generador, como en M0: úsalo para comparar con corridas
viejas, que no tienen radio vertical.

El mundo se elige con `--world`: `forest` (por defecto, el de todas las corridas anteriores),
`terrain`, `flat` u `overworld` (piedra infinita hacia abajo con cuevas). Con el overworld,
`--caves mc|closed|off` elige las cuevas: `mc` tiene entradas desde la superficie y `closed` no
deja ver ninguna cueva desde arriba. El overworld no acepta `--fixed-height` (no tiene piso) y usa
un arena de 24 Mi quads y 65 536 slots; `--arena-quads N` y `--chunk-slots N` los cambian. El
resumen anota el mundo y el arena y los slots que el renderer obtuvo de verdad (los límites del
dispositivo pueden recortarlos), y `tools/bench_report.py` solo compara corridas del mismo mundo.

Los generadores de esos mundos (salvo `flat`) son contenido de ejemplo y viven en `boxel_demo`
(#48). Sus benches de criterion son `cargo bench -p boxel_demo --bench terrain` y
`cargo bench -p boxel_demo --bench caves` (`just bench terrain` también sirve).

`--water Y` llena de agua todo el aire a la altura `Y` o más abajo (lagos y mares) y pone un bloque
de vidrio en la superficie de una de cada 29 columnas de agua: chunks con quads translúcidos para
medir el pase translúcido y su orden (ADR 0018). El forest tiene la superficie entre 16 y 112, así
que `--water 62` cubre más o menos la mitad. El resumen anota el mundo como `forest+water62` y
`tools/bench_report.py` no lo mezcla con el forest seco. `--sort off` deja los quads translúcidos
de cada chunk en el orden del mesher (`TranslucentSort::Off`); el resumen no lo anota, así que
conviene ponerlo en `--label`. La corrida imprime cuántos buckets translúcidos reordenó mientras
medía.

`--light off` apaga la luz de vóxeles y la oclusión ambiental: el mundo se ve y cuesta como antes
de la luz (por defecto es `on`). El resumen lo anota y `tools/bench_report.py` no mezcla corridas
con luz distinta. Las corridas viejas sin el dato cuentan como `on` si anotan la hora (llegó con
la luz) y como `none` si son de antes de la luz.

`--cave-culling off` (o `--no-cave-culling`) dibuja todo lo que pasa el frustum, como antes de la
ocultación de cuevas (ADR 0022; por defecto está encendida). El resumen lo anota (el título dice
`culling off`) y trae una sección "Cave culling": chunks ocultos, chunks que pasan el culling en
CPU, draws, y cuántos frames buscaron y cuánto tardaron. `tools/bench_report.py` no separa las
corridas por esta opción (así una rama con culling se compara con las corridas de main), pero avisa
si difiere y compara esos números.

**Bajo tierra, la ocultación de cuevas se mide con la cámara en una cueva (`--cave`).** La cámara
de `stress` no choca: con el ojo dentro de la roca el render ve a través de ella y el culling no
oculta nada (una cámara como `16,-1000,16` está en la piedra). `--cave` (solo en el overworld)
mueve el inicio al vóxel de aire más cercano a `--camera` con 2 vóxeles de aire alrededor, 16 o
más bajo la superficie y un camino de 64 vóxeles o más así. Es determinista. El escenario `path`
(`just bench-path`) sigue ese camino ida y vuelta a `--speed` mirando hacia adelante, sin salir del
espacio abierto; `fly` y `dive` atraviesan la roca y `--cave` no los acepta. El resumen anota la
cámara y la cueva (el título termina en `cave`) y avisa si en más de 1 % de los frames el ojo
estuvo en la roca. Ejemplo:
`just bench-path --world overworld --cave --camera 656,-1000,16,-90,0 --speed 8`.

Para contar draws y caras con y sin ocultación de cuevas, sin GPU ni ventana y sin ruido:
`headless_world --world overworld --view-distance 16 --hz 0 --cull --camera X,Y,Z,YAW,PITCH`
(con `--cave` bajo tierra: la línea `cull:` debe decir `open=yes`).
El costo de la búsqueda y de los enlaces de caras está en
`cargo bench -p boxel_world --bench cave_culling`.

`--pack DIR` usa las texturas de un resource pack. El resumen anota el pack (`none` con las
procedurales, también en corridas viejas) y cuánto tardó en cargar, y `tools/bench_report.py` no
mezcla corridas con texturas distintas.

`--fog PRESET` agrega la niebla y las nubes del ejemplo (ADR 0030) con `FogPlugin`; sin la opción
(o con `off`) el plugin no está y no hay niebla ni costo. Los presets son los de
`boxel_demo::fog`:

- `empty`: el plugin sin capas. Mide solo lo que cuesta poder leer la profundidad.
- `valley`: bancos de cubos de 2 bloques entre y 48 y 64, sobre el mar y las zonas bajas.
- `clouds`: nubes de 12x4x12 a y 192.
- `both`: valle y nubes, el preset típico de un juego.
- `inside`: un manto de y 0 a 256 con la cámara adentro, el peor caso.
- `sunset`: valle y nubes con más color del cielo.

`--fog-res half|full` elige la resolución de la marcha (media por defecto) y `--fog-steps N` el
tope de celdas por rayo (64 por defecto, el del motor). El resumen anota la niebla
(`fog: valley half`, con los pasos si no son los de siempre) y `tools/bench_report.py` empareja
igual corridas con y sin niebla, así se puede comparar una contra otra; solo avisa que difieren.
Para medir el costo de la niebla con el mismo binario:
`just perf-smoke --base HEAD "--new-extra='--fog both'" rtx/surface/r16/uncapped`.

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
| `dive` | baja en línea recta a velocidad fija | los tirones al entrar capas nuevas por debajo |
| `spin` | gira 360 grados en su lugar | el costo de que cambie lo que se ve, sin chunks nuevos |
| `path` | con `--cave`, recorre una cueva ida y vuelta mirando hacia adelante | la ocultación de cuevas mientras se camina bajo tierra |

Todos esperan a que el mundo termine de cargar, corren `--warmup` segundos sin medir y después
miden `--duration` segundos.

Sin `--fps-limit`, el bucle corre sin límite de fps y dibuja en cada vuelta, aunque la ventana
esté tapada. Los fps de acá dicen cuántos frames puede hacer el motor, no cuántos muestra el
monitor: solo se presenta un frame por cada refresco del monitor (unos 144 por segundo) y el resto
se dibuja sin mostrarse.

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
| border catch-up | después de cada cruce, cuánto tardó el mundo en terminar los chunks nuevos. Si no termina antes del próximo cruce, va atrasado. Si la medición corta el último cruce a la mitad, ese no cuenta. Se mide con el reloj de los frames (cuándo empieza el frame que ya ve el mundo quieto): con límite sale en períodos enteros. También dice cuántos frames tardó |
| border work lag | lo mismo, pero hasta que terminó el último job del cruce, sin redondear a frames. Separa lo que tarda el mundo de lo que agrega el ritmo de los frames |
| Between frames | con límite: tiempo y pasos del schedule `Idle` por frame (media y p99), y los pasos que terminaron después de su ventana (deberían ser 0; el peor se anota) |
| catch-up after stop | cuánto tardó en terminar todo después de que la cámara frenó |
| Worst 5 frames | los 5 peores, con lo que pasó en cada uno (cruce, chunks generados, mallas, cola) y si se presentó |
| > 7.5 ms | frames que se notan con el límite de 144 (el período es 6.94 ms) |
| Pacing | el intervalo entre frames, entre frames presentados y entre frame callbacks del compositor (mediana, p99, máximo, desvío y cuántos pasan de 1.5 veces la mediana: con un frame por refresco, cada uno es un refresco sin frame nuevo). También los frames que el runner retuvo esperando la compuerta de presentación y los que se rindieron y corrieron sin presentar |
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
mundo, luz (`--light`), texturas (`--pack`), escenario, radio, radio vertical, velocidad, límite de fps, vsync, cuerpos
(`--bodies`), ediciones (`--edits`), cueva (`--cave`), backend, tier y GPU. Una hora (`--time`) o
una cámara distinta solo se anota.
Si las corridas tienen los datos de ritmo (Pacing), también los compara.

Para una tarea alcanza con `just perf-smoke` de los casos que toca. Para investigar a mano:
`just bench-all` antes del cambio, mover esas corridas (los `bench-results/*.md` y `*.csv`) a otra
carpeta, aplicar el cambio, `just bench-all` otra vez y comparar las dos carpetas. No muevas ni
borres `bench-results/bins/`, `control/`, `smoke/` ni `bisect/`: son del harness, y
`control/last` dice contra qué compara la próxima ronda de control.

## El CSV

Una fila por frame: `frame_ms`, `crossed` (cruzó un borde), `generated`, `meshes`, `removed`,
`queued_jobs`, `in_flight`, los contadores de `WorldStats` y la posición de la cámara.
`arena_used` son las entradas del arena del render en uso (quads más paletas de luz) y
`dropped_chunks` los chunks que el render no pudo guardar hasta ese frame (arena o tabla llenos).
`gate_wait_ms` es lo que el runner retuvo el frame esperando la compuerta de presentación,
`gate_missed` si se rindió y `gate_ms` cuándo se abrió la compuerta por última vez (el frame
callback del compositor en Wayland), en ms desde el inicio de la medición. `display_locked` dice
si el runner marcó el ritmo con el monitor (ADR 0017). `idle_ms` e `idle_steps` son el tiempo y
los pasos del schedule `Idle` después del frame, `idle_late_ms` cuánto se pasó de su ventana el
peor de ellos e `idle_overruns` cuántos se pasaron. `job_end_ms` es cuándo terminó el último job
del mundo hasta ese frame, en ms desde el inicio de la medición.
El resumen anota los dos al final. Sirve para graficar o para buscar qué pasó en un tirón
puntual.

## Tiempo por sistema (--spans)

Para saber en qué se va el tiempo de un frame, sistema por sistema:

```bash
cargo run --release --features trace --example stress -- --scenario fly --speed 64 \
    --radius 32 --summary out.md --spans --spans-csv out-spans.csv
```

Con `--features trace` cada sistema del ECS abre un span de `tracing`, y el mundo y el render
agregan spans propios por dentro (`world.positions`, `world.drain`,
`world.dispatch`, `render.upsert`, `render.submit`...). `world.step create`, `unload`, `update`,
etc. reparten el tiempo de `world.positions` por tipo de paso (`world.step batch` es llenar cada
tanda de 32 posiciones y buscarlas en el mapa, antes de sus pasos). `--spans` suma el tiempo de cada
uno en cada frame y agrega al resumen la sección "Time per system":

- media, p99 y máximo por sistema en tres grupos de frames: cruce de borde, puesta al día (los
  frames después del cruce hasta que el mundo termina) y el resto;
- el total por cruce de borde, sumando el cruce y su puesta al día, de los cruces en que el mundo
  terminó antes del siguiente (dice cuántos de cuántos);
- los 10 peores frames de cruce, con lo que más tardó en cada uno.

Una suma por cruce crece con los frames que dura el cruce aunque cada frame cueste lo mismo: si
los frames se vuelven más rápidos, el cruce tiene más frames y sistemas que no cambiaron parecen
más caros. Por eso la tabla trae "above normal", la suma menos lo que esos frames cuestan en un
frame normal (frames por cruce por la media de los frames normales): el trabajo extra del cruce.
Al comparar dos versiones, mirar esa columna junto con los frames por cruce y cuántos cruces
terminaron (una versión que tarda más en ponerse al día deja afuera más cruces pesados).

`--spans-csv` deja lo mismo frame a frame. Para comparar dos versiones, las dos tienen que estar
compiladas con `--features trace`, porque los spans también cuestan tiempo: en radio 32, cerca de
un 1 % de fps, y unos 50 ns más por posición en `world.positions`.

Detalles:

- Sin `--features trace` el flag da error y los spans no existen en el binario. Con la feature
  pero sin `--spans`, los spans igual cuestan (los recibe el log), solo que nadie los suma.
- Los sistemas que corren en paralelo suman más que el frame. Las filas `schedule` son tiempo
  real en el hilo principal, y `outside Main` es lo que pasa fuera de `App::update` (winit). Los
  pasos entre frames caen ahí: su fila es `schedule Idle` (con `world.idle` adentro) y cuentan
  para el frame que terminó antes de ellos.
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
