# Física P3: colisiones entre cuerpos (2026-10-06)

Qué se midió: el costo de la fase P3 ([ADR 0016](../adr/0016-fisicas-propias.md)) con
`crates/boxel_physics/benches/physics.rs`. Un `tick` es una corrida de `FixedPostUpdate` con
`PhysicsPlugin` sobre el bosque: ahora incluye los obstáculos, el broadphase, el solve y los
eventos. `grid/N` es solo la grilla del broadphase: armarla con N cuerpos y sacar los pares
candidatos.

## Resultado

| Bench | Objetivo | P2 | P3 |
|---|---|---|---|
| `tick/0` | 20 µs | 0.48 µs | 0.86 µs |
| `tick/100` | 0.05 ms | 24.4 µs | 33.4 µs |
| `tick/1000` | 0.5 ms | 0.234 ms | 0.403 ms |
| `grid/1000` | sin objetivo | | 25.2 µs |
| `grid/10000` | 0.3 ms | | 0.480 ms |
| `grid/10000_crowd` | sin objetivo | | 1.205 ms |

Los ticks siguen bajo su objetivo. El de 1000 cuerpos sube 0.17 ms: los 700 caminantes son
sólidos y ahora se empujan entre ellos, y cada paso arma la grilla, junta los cuerpos con
collider (ordenados por `Entity`) y prueba los pares.

La grilla de 10 000 cuerpos no llega a los 0.3 ms del plan: cuesta 0.48 ms con los cuerpos tan
repartidos como en los ticks (uno cada 9.2 bloques de suelo, 1850 pares candidatos) y 1.2 ms con
todos apretados en 128 x 128 bloques (unos 10 cuerpos por celda de 4 x 4, 10 498 pares). Con
1000 cuerpos cuesta 25 µs.

## Qué se probó para bajar la grilla

Con 10 000 cuerpos repartidos, en una corrida larga: insertar 90 µs, ordenar las entradas
(counting sort) 110 a 140 µs y recorrer los pares 230 a 260 µs.

- Sacar el margen de los límites de cada cuerpo: un cuerpo apoyado justo sobre el borde de una
  celda (el suelo a una altura múltiplo de 4) ocupaba dos celdas en Y. Bajó las entradas de 24 000
  a 15 500 y el total de 0.85 a 0.50 ms. Quedó.
- Cuerpos compactos (una línea de caché, límites relativos a su celda en `i32`) y entradas de 8
  bytes. Quedó: bajó un poco el tiempo y la memoria.
- Copiar los datos del cuerpo en cada entrada ordenada, probar los pares sin saltos de rama,
  recorrer solo los buckets con dos entradas o más y usar más buckets: ninguno cambió el total
  más allá del ruido, así que no quedaron.

Recorrer las mismas entradas en orden cuesta 9 µs: el tiempo se va en el bucle de pares, no en
la memoria. Sin `perf` (el sistema no lo permite sin cambiar `perf_event_paranoid`) no se pudo
ver más. Lo que falta para llegar a 0.3 ms es una grilla incremental: en P4 los cuerpos dormidos
no se vuelven a insertar en cada paso.

## Cómo se midió

- Rama `physics/p3p4`, perfil `bench` (release con LTO thin), un hilo, configuración por
  defecto de criterion.
- AMD Ryzen 7 250 (16 hilos), Rust 1.98.1, criterion 0.8, gobernador `powersave`.
- `tools/wait-idle.sh 60` pasó justo antes de cada corrida (CPU ocupada a lo sumo un 15 %). En la
  misma corrida, `sweep/player_walk` dio 0.155 µs y `raycast/sky_1024` 7.0 µs, como en P1 con la
  máquina quieta.
