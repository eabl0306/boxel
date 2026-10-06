# Física P2: un paso con cuerpos (2026-10-06)

Qué se midió: el paso de física completo de la fase P2 ([ADR 0016](../adr/0016-fisicas-propias.md)),
con `crates/boxel_physics/benches/physics.rs`. Un `tick` es una corrida de `FixedPostUpdate` con
`PhysicsPlugin`: controlador, preparar, cinemáticos, el paso de los cuerpos dinámicos, eventos y
la escritura de `Transform`. El mundo es el bosque alrededor del origen (radio 2), leído por
`ChunkSource` como en el juego.

## Resultado

Todos quedan bajo su objetivo.

| Bench | Qué hace | Objetivo | Medido |
|---|---|---|---|
| `tick/0` | un paso sin cuerpos | 20 µs | 0.50 µs |
| `tick/100` | 100 cuerpos despiertos | 0.05 ms | 24 µs |
| `tick/1000` | 1000 cuerpos despiertos | 0.5 ms | 0.27 ms |

Son unos 0.24 a 0.27 µs por cuerpo y paso: el barrido contra los vóxeles (0.1 a 0.15 µs en P1)
más el controlador, la preparación y la escritura de `Transform`.

Los cuerpos: el 70 % son caminantes con `CharacterController` que dan la vuelta cada 64 pasos
(uno de cada cuatro salta entonces) y vuelven a su lugar cada 1024 pasos, para no salir del
mundo cargado en una medición larga. El 30 % son objetos soltados desde hasta 20 bloques, en
reposo durante la medición; el paso los barre igual porque todavía no duermen (eso llega en
P4). Ninguno quedó congelado.

## El ejecutor de `FixedPostUpdate`

| Bench | Multi-hilo | Un hilo |
|---|---|---|
| `tick/0` | 24.9 µs | 0.65 µs |
| `tick/100` | 52.3 µs | 23.4 µs |
| `tick/1000` | 0.32 ms | 0.22 ms |

Los sistemas de física van encadenados. Repartirlos entre hilos no gana nada: cuesta unos 24 µs
fijos por paso y pierde caché entre un sistema y el siguiente. Con el ejecutor multi-hilo,
`tick/0` y `tick/100` no cumplían. Por eso `PhysicsPlugin` pone `FixedPostUpdate` en el ejecutor
de un hilo (se apaga con `single_threaded: false`). El paralelismo que sí sirve, por cuerpo
dentro del paso (`par_iter_mut`), llega en P4.

## Cómo se midió

- Rama `physics/p2`, perfil `bench` (release con LTO thin), un hilo.
- AMD Ryzen 7 250 (16 hilos), Rust 1.98.1, criterion 0.8. La tabla de resultados usa la
  configuración por defecto; la comparación de ejecutores, 2 s de calentamiento y 4 s de
  medición.
- `tools/wait-idle.sh` pasó antes de cada corrida, pero la máquina siguió en uso por otros
  trabajos (navegador y compilaciones). En la misma corrida, `sweep/player_walk` dio 0.27 µs y
  `raycast/sky_1024` 10.1 µs, contra 0.14 y 7.0 µs en P1 con la máquina quieta, así que estos
  números pueden estar inflados hasta unas 1.5 veces. Una segunda corrida con la CPU ocupada en
  promedio un 73 % dio 0.74 µs, 49 µs y 0.23 ms: con tanta carga `tick/100` queda justo en el
  límite. Falta repetirlos con la máquina quieta.

## Después de la revisión (2026-10-06)

Cambios que tocan el costo: los cuerpos del bench ahora son `Interpolated` (como el jugador y
los objetos de los ejemplos), `PhysicsPlugin` también pone `RunFixedMainLoop` en el ejecutor de
un hilo, los impactos se ordenan sin memoria extra (`sort_unstable_by_key`) y una velocidad por
debajo de `STOP_SPEED` queda en 0. El bench nuevo `frame/1000` mide un frame sin paso a 144 fps:
`RunFixedMainLoop` con la interpolación de los 1000 cuerpos, que corre en cada frame (144 veces
por segundo, contra 64 pasos).

| Bench | Objetivo | Medido |
|---|---|---|
| `tick/0` | 20 µs | 0.48 µs |
| `tick/100` | 0.05 ms | 24.4 µs |
| `tick/1000` | 0.5 ms | 0.234 ms |
| `frame/1000` (un hilo) | sin objetivo | 5.1 µs |
| `frame/1000_multi_thread` | sin objetivo | 13.6 µs |

El ejecutor multi-hilo sumaba unos 8.5 µs por frame en `RunFixedMainLoop` (1.2 ms por segundo a
144 fps); con el de un hilo, interpolar 1000 cuerpos cuesta 5 ns por cuerpo. Mismo equipo y
perfil que arriba, configuración por defecto de criterion. `tools/wait-idle.sh 60` pasó justo
antes (CPU ocupada a lo sumo un 15 %). Estos son los números de referencia de P2 (`tick/1000`
ahora incluye la interpolación).
