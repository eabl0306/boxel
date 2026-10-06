# Física P1: barrido y raycast (2026-10-06)

Qué se midió: el núcleo de `boxel_physics` de la fase P1 ([ADR 0016](../adr/0016-fisicas-propias.md)),
con `crates/boxel_physics/benches/physics.rs`. Todo lee el mundo a través de `ChunkSource`, como
la capa ECS: una búsqueda de chunk por chunk tocado y los chunks uniformes sin decodificar.

## Resultado

Todos los benches quedan bajo su objetivo, con margen.

| Bench | Qué hace | Objetivo | Medido |
|---|---|---|---|
| `sweep/player_walk` | caja de jugador (0.6 x 1.8 x 0.6) caminando sobre el bosque, un paso de 64 Hz con gravedad | 0.3 µs | 0.142 µs |
| `sweep/player_land` | la misma caja cae medio bloque y aterriza | 0.3 µs | 0.124 µs |
| `sweep/player_fall` | cae medio bloque por el aire | 0.3 µs | 0.088 µs |
| `raycast/pick_6` | apuntar desde el ojo a 45 grados hacia abajo, alcance 6 | 0.5 µs | 0.091 µs |
| `raycast/cave_256` | 256 bloques bajo tierra entre piedra y cuevas, sin aceptar ningún bloque (el peor caso) | 5 µs | 1.93 µs |
| `raycast/sky_1024` | 1024 bloques en diagonal por el cielo (chunks uniformes de aire) | 10 µs | 7.01 µs |

El raycast en el cielo cumple sin saltar chunks de aire enteros (unos 4.8 ns por bloque cruzado),
así que ese atajo no se agregó: el plan lo pedía solo si este bench lo necesitaba.

Cuenta por cuerpo: con unos 0.14 µs por barrido, 1000 cuerpos despiertos son unos 0.14 ms por
paso de física solo en colisión con vóxeles, dentro del presupuesto de 0.5 ms de P2.

## Cómo se midió

- Commit b1d9a61 (rama `physics/p0p1`), perfil `bench` (release con LTO thin), un hilo.
- AMD Ryzen 7 250 (16 hilos), Rust 1.98.1, criterion 0.8 con su configuración por defecto.
- Los mundos se generan una vez antes de medir, con `WorldPlugin` y trabajos en línea: bosque
  alrededor del origen (259 chunks), cielo encima del bosque a radio 17 (3135 chunks) y una fila
  de chunks del overworld con cuevas a y = -200 (108 chunks).
- Dos corridas seguidas, después de `tools/wait-idle.sh`. La CPU estuvo ocupada en promedio un
  17 % durante cada corrida (el bench usa un hilo, un 6 %; el resto era otro trabajo de la
  máquina). Las dos corridas dieron lo mismo con menos de 3 % de diferencia; la tabla es la
  segunda. Una corrida anterior con la máquina más cargada dio hasta 1.5 veces más en
  `player_fall`, `pick_6` y `cave_256`, y no se usa.
