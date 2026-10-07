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

Ojo: esta tabla se midió con el paso en paralelo (la columna P4 de `10000_asleep`,
`1000_projectiles` y los `tick`, que pasan los 256 cuerpos despiertos), y los objetivos del plan
son en un hilo salvo el de 10 000 despiertos. La revisión de abajo repite la medición en un
hilo: `10000_asleep` no llega en un hilo (0.637 ms), y la baja de `tick/1000` se debía sobre
todo al paso en paralelo, no a dormir.

El de 10 000 cuerpos despiertos no llega: cuesta 2.06 ms y el plan pedía 1.5 ms.

`tick/0` sube 0.6 µs por los sistemas nuevos (despertar, proyectiles, dormir), lejos de su
objetivo.

## Revisión: en un hilo, como piden los objetivos (2026-10-06)

Después de las correcciones de la revisión de P4 (ver el ADR). Los benches nuevos: las
variantes `_one_thread`, `10000_asleep_chunk_events` (16 cargas de chunk por paso entre los
dormidos, en chunks que ninguno alcanza) y `sleep_churn` (240 de 2000 cajas dormidas se duermen
a mano y se despiertan, un paso cada cosa). Los `tick` ahora corren en un hilo.

| Bench | Objetivo | En un hilo | Con el paso en paralelo |
|---|---|---|---|
| `scale/10000_asleep` | 0.6 ms | 0.637 ms (no llega) | 0.447 ms |
| `scale/10000_asleep_chunk_events` | sin objetivo | 0.650 ms | |
| `scale/sleep_churn` | sin objetivo | 75 µs | |
| `scale/1000_projectiles` | 0.3 ms | 0.248 ms | 0.156 ms |
| `scale/10000_awake` | 1.5 ms (en paralelo) | 4.76 ms | 2.08 ms (no llega) |
| `tick/0` | 20 µs | 1.52 µs | |
| `tick/100` | 0.05 ms | 28.5 µs | |
| `tick/1000` | 0.5 ms | 0.355 ms | |

- `10000_asleep` en un hilo se pasa 6 %: los 1000 caminantes cuestan 0.34 ms de movimiento, lo
  de entre cuerpos 0.28 ms y el resto 0.11 ms. Con el paso en paralelo, como corre un juego,
  queda en 0.447 ms. Para cumplirlo en un hilo habría que achicar el movimiento de los
  caminantes o lo de entre cuerpos; queda para el dueño decidir si el objetivo pasa a ser con el
  paso en paralelo.
- `tick/1000` en un hilo baja de 0.403 ms (P3) a 0.355 ms: 12 % menos con 27 % de cuerpos
  dormidos. Los 0.26 ms de la primera tabla eran con el paso en paralelo.
- 16 cargas de chunk por paso cuestan unos 13 µs con 9000 dormidos: cada una busca sus
  dormidos en el índice de chunks en vez de recorrerlos todos.
- Detectar el empujón a mano (`BodyPosition` escrito por el juego) guardando la última
  posición en `PhysicsSync` costaba de 3 a 6 % en un hilo (`10000_awake_one_thread`,
  `1000_projectiles_one_thread`, comparado contra el código anterior en la misma máquina). Se
  cambió por el tick de cambio y quedó igual que antes: 4.60 contra 4.66 ms y 241.0 contra
  241.5 µs.

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
  quieta. En la de la revisión, 0.162 µs y 6.95 µs.
