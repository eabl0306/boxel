# Física P4: dormir, proyectiles y paso en paralelo (2026-10-06)

Qué se midió: el costo de la fase P4 ([ADR 0016](../adr/0016-fisicas-propias.md)) con
`crates/boxel_physics/benches/physics.rs`. Un paso es una corrida de `FixedPostUpdate` con
`PhysicsPlugin` sobre el bosque. Los benches `scale/*` son nuevos: 10 000 cuerpos tan repartidos
como en los `tick` (uno cada 9.2 bloques de suelo, en un cuadrado de 304 bloques) y 1000 flechas.
Las corridas con ventana de stress (`--bodies`) quedan para la medición.

## Resultado

| Bench | Objetivo | P3 | P4 |
|---|---|---|---|
| `scale/10000_asleep` (9000 objetos dormidos, 1000 caminantes) | 0.6 ms | | 0.465 ms |
| `scale/10000_awake` (todos despiertos, en los hilos) | 1.5 ms | | 2.06 ms (no llega) |
| `scale/10000_awake_one_thread` (lo mismo en un hilo) | sin objetivo | | 4.76 ms |
| `scale/1000_projectiles` (1000 flechas, 100 caminantes) | 0.3 ms | | 0.158 ms |
| `tick/0` | 20 µs | 0.86 µs | 1.47 µs |
| `tick/100` | 0.05 ms | 33.4 µs | 28.4 µs |
| `tick/1000` | 0.5 ms | 0.403 ms | 0.260 ms |
| `grid/10000` | 0.3 ms | 0.480 ms | 0.479 ms (no llega) |

Dos de los tres objetivos nuevos se cumplen. El de 10 000 cuerpos despiertos no: cuesta 2.06 ms
y el plan pedía 1.5 ms.

Los `tick` bajan porque los objetos tirados ahora se duermen: en `tick/1000` quedan 730 cuerpos
despiertos de 1000. `tick/0` sube 0.6 µs por los sistemas nuevos (despertar, proyectiles,
dormir), lejos de su objetivo.

## Dónde se va el tiempo

Cada bench imprime el desglose de un paso promediado en 128 pasos: el movimiento de los cuerpos
(`step_time`), lo de entre cuerpos (broadphase y solve, `solve_time`) y el resto.

| Bench | Paso | Movimiento | Entre cuerpos | Resto |
|---|---|---|---|---|
| `10000_asleep` | 0.47 ms | 0.11 | 0.25 | 0.10 |
| `10000_awake` | 1.94 ms | 0.45 | 1.28 | 0.21 |
| `10000_awake_one_thread` | 4.77 ms | 2.85 | 1.55 | 0.37 |
| `1000_projectiles` | 0.12 ms | 0.07 | 0.01 | 0.04 |

Con 10 000 cuerpos despiertos el movimiento ya va en paralelo (6.3 veces más rápido que en un
hilo, con 7 hilos de cómputo). Lo que falta es lo de entre cuerpos, que sigue casi todo en un
hilo. Medido con marcas de tiempo dentro de los sistemas:

| Parte | ms |
|---|---|
| juntar los 10 000 cuerpos de la query | 0.22 |
| meterlos en la grilla | 0.17 |
| ordenar las celdas y barrer los pares (el barrido en paralelo) | 0.21 |
| ordenar los pares por entidad | 0.04 |
| empujes (508 por paso, en orden de abajo hacia arriba) | 0.16 |
| escribir los cuerpos que cambiaron | 0.07 |
| rehacer la grilla para las consultas | 0.10 |

## Qué se probó

Los pasos de `scale/10000_awake`, de la primera versión a la final:

- Ordenar la lista entera por entidad costaba 0.35 ms (cada cuerpo pesa casi 300 bytes). Ahora
  se ordenan solo los pares. Junto con calcular los límites de cada cuerpo una sola vez y no
  buscar dormidos cuando no hay: de 3.4 a 2.6 ms.
- `Prepare`, el controlador y el `Writeback` en paralelo: el resto bajó de 0.41 a 0.21 ms, el
  paso a 2.3 ms.
- El barrido de pares de la grilla repartido por rangos de celdas (de 0.36 a 0.2 ms), recorrer en
  el solve solo los cuerpos que tocó y no copiar los pares: 2.2 ms. Con pocos cuerpos las tareas
  cuestan más de lo que ahorran, así que el barrido en paralelo solo se usa desde 4096.
- Los límites de los cuerpos en tandas por hilo: 2.0 ms.
- `Sleeping` como componente de tabla en vez de sparse set (las queries con `Without<Sleeping>`
  vuelven a ser densas): no cambió nada fuera del ruido, así que quedó en sparse set (despertar
  y dormir no mueven al cuerpo de tabla).

Para llegar a 1.5 ms habría que paralelizar lo que queda en un hilo: juntar los cuerpos y
meterlos en la grilla en tandas, y no rehacer la grilla entera después de los empujes. También
ayudaría una grilla incremental (la de P3 tampoco llega a sus 0.3 ms) y achicar el cuerpo
indexado. Con 90 % de los cuerpos dormidos, el caso de la regla del frame, el paso ya cumple.

## Cómo se midió

- Rama `physics/p3p4`, perfil `bench` (release con LTO thin), criterion 0.8 con su configuración
  por defecto.
- AMD Ryzen 7 250 (16 hilos, 7 de cómputo), Rust 1.98.1, gobernador `powersave`.
- `tools/wait-idle.sh 60` pasó justo antes de la corrida. En la misma corrida,
  `sweep/player_walk` dio 0.153 µs y `raycast/sky_1024` 7.1 µs, como en P1 y P3 con la máquina
  quieta.
