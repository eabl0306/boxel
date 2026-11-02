# ADR 0060: filtros de luz por canal (vidrio de color)

- Estado: aceptado (issue #11, el punto del vidrio de color; fase P8 del plan de luz)
- Fecha: 2026-11-02

## Contexto

La luz de bloque tiene tres canales de 4 bits (rojo, verde y azul) y la del cielo uno solo (ADR
0015). El dueño quiere que la luz que pasa por un vidrio de color tome su color: una antorcha
detrás de un vitral rojo ilumina rojo el cuarto de al lado (plan de luz, P8; issue #11).

Minecraft no lo hace: su vidrio de color deja pasar la luz igual que el aire. Es una diferencia
buscada, del motor, que el juego usa.

Lo que pesaba:

- **El camino caliente.** La luz de un chunk se calcula en cada job de malla. Un mundo sin vidrio
  de color no puede pagar casi nada.
- **La exactitud de la caja.** `box_light.rs` calcula la luz de un chunk desde una caja de ±16
  vóxeles y da la solución global bit a bit porque la luz cae al menos 1 por vóxel desde 15. La
  regla nueva tiene que mantener eso, y el punto fijo tiene que seguir siendo único.
- **Los tres tiers.** Nada de render puede cambiar donde no hay filtros.
- **El motor no nombra contenido.** El motor da el mecanismo; el juego pone los colores.

## Decisión

### La regla

- `BlockRegistryBuilder::set_light_filter(block, [r, g, b])` pone un filtro a todos los estados de
  un bloque: la fracción del brillo de cada canal que pasa, en quinceavos (`0..=15`). `[15, 15,
  15]` es sin filtro. Se lee con `BlockRegistry::light_filter` y `LightTables::light_filter`,
  `filters` y `any_filter`.
- Al entrar a un vóxel con filtro, cada canal de bloque vale `transmit(l - costo, f)` en lugar de
  `l - costo`, con `costo = max(1, opacidad)` como siempre.
- `transmit(l, f)` es el nivel cuyo brillo está más cerca de `f / 15` del brillo del nivel `l`
  (los empates van hacia abajo), en la curva base de Minecraft `l / (60 - 3l)`, la misma que
  dibuja `boxel_render::light_curve` con gamma 0. Es una tabla de 16 por 16 que se arma al
  compilar con enteros: da lo mismo en cualquier máquina.
- Varios vidrios se suman como filtros reales: rojo y después azul dejan casi nada.
- Un estado opaco (opacidad 15) ignora su filtro: la luz no entra.

### Qué hace el filtro a la luz de bloque

Tres formas, con el mismo costo (una tabla o una cuenta por vóxel con filtro):

| Forma | Rojo de Minecraft (`[10, 3, 2]`) sobre una lámpara a 14 | Problema |
|---|---|---|
| Restar niveles por canal | depende de cuánto se resta; la fracción que pasa cambia con el nivel de entrada | no respeta el color del tinte |
| Escalar el nivel, `l * f / 15` | rojo 9 (brillo 0,27 en vez de 0,52) | oscurece de más la luz fuerte: 10/15 deja la mitad de la luz |
| **Escalar el brillo (elegida)** | rojo 12 (brillo 0,5 contra 0,52 pedido) | ninguno que se vea; la tabla es por nivel entero |

El color del tinte es una transmitancia: dice qué parte de la luz pasa. Escalar el brillo es lo
que hace un vidrio de verdad.

### El cielo queda blanco

La luz del cielo no pasa por ningún filtro. Las opciones:

1. **Blanco (elegida).** Costo cero. El vidrio de color sigue con opacidad 0, así que el cielo
   directo pasa como hoy. De día, un cuarto detrás de un vitral se ve con luz blanca. De noche, o
   con una antorcha, la luz de bloque sí se tiñe.
2. **Cielo de color.** Tres canales de cielo más por vóxel: `LightValue` pasa de 16 a 28 o 32
   bits. Eso duplica `PaddedLight` (77 KiB a 154 KiB por job), `ChunkLight`, las esquinas de la
   paleta de luz de cada malla y su formato en la GPU de los tres tiers, y el shader tiene que
   multiplicar el color del cielo por canal. Toca render y shaders protegidos, y la memoria de
   luz de todo el mundo, por un efecto que solo se ve de día detrás de un vitral.
3. **Pasar el cielo filtrado a la luz de bloque.** Barato, pero deja de seguir el día: de noche la
   ventana seguiría iluminando. No es honesto.
4. **Solo oscurecer el cielo, sin color.** El vidrio tendría que contar para el techo de cielo de
   su columna (opacidad mayor que 0), lo que cambia la caminata del cielo y hace oscuro todo lo
   que hay debajo de un techo de vidrio. Gana poco.

### La exactitud de la caja sigue valiendo

- `transmit` nunca sube un nivel, así que la luz sigue cayendo al menos 1 por vóxel y empieza en
  15 como máximo. Toda la luz que llega al chunk sale de una fuente a 14 vóxeles o menos: la
  caja de ±16 la contiene.
- La regla es monótona: más luz que entra nunca da menos luz que sale. El punto fijo sigue siendo
  único y la cola por niveles, de mayor a menor, sigue sacando cada vóxel en su nivel final.
- El oráculo (`oracle.rs`) aplica la misma regla. Las pruebas comparan la caja con él bit a bit
  en mundos al azar con filtros (con y sin opacidades 2 a 14, cerca y lejos del origen) y en una
  escena hecha a mano: una lámpara detrás de vidrio rojo y de agua cian, y un pozo bajo un techo
  de vidrio rojo, donde el cielo llega blanco.

### Cómo se calcula

- `LightMasks::filter`: los vóxeles con filtro de un chunk. Sale en un pase aparte
  (`filter_words`) y solo si la paleta del chunk tiene un estado con filtro. Un chunk sin vidrio
  de color paga un escaneo de paleta, y nada si ningún estado del registro filtra.
- En la caja, si hay emisores y alguno de los 27 chunks tiene filtro, se juntan sus filas (más
  las parciales, si hay opacidades 2 a 14) y las inundaciones de bloque toman esos vóxeles uno
  por uno: leen el estado, su costo y su filtro. La inundación del cielo no cambia. Sin filtros
  cerca, el camino es el de antes.
- Las inundaciones quedan en una función aparte (`floods`), fuera de línea, para que los caminos
  rápidos sigan siendo una función chica.
- Nada cambia en la malla, el quad, la paleta de luz ni los shaders: los quads ya llevan luz RGB
  por esquina. Por eso los tres tiers no necesitan nada.

## El juego

- Los 16 vidrios de color y sus paneles filtran con el color de su tinte: el
  `DyeColor.getTextureDiffuseColor` de Minecraft, el mismo que tiñen los rayos del faro y los
  estandartes, pasado a quinceavos y redondeado. El blanco da `[15, 15, 15]`: no filtra. El rojo
  da `[10, 3, 2]`, el celeste `[3, 11, 13]` y el negro `[2, 2, 2]`, que casi corta la luz.
- El vidrio polarizado (tinted glass) sigue con opacidad 15, como en Minecraft: no deja pasar
  luz, ni del cielo ni de bloque, aunque se vea a través. No necesita filtro, porque un estado
  opaco no deja entrar luz. Es el caso extremo de un filtro `[0, 0, 0]`, que además corta el
  cielo.
- El vidrio común y su panel no filtran.

## Medidas

- `headless_world --view-distance 16` (overworld y bosque): los mismos chunks, mallas, quads
  (5.023.105 y 933.665), entradas del arena y caminos de luz que main. Ningún mundo de ejemplo
  tiene filtros.
- Criterion `light`, una corrida por lado contra main (1672cbd). La máquina varía unos ±6 % de
  una corrida a otra: el mismo binario de main, corrido otra vez, dio +6 % en `box_cave_dark` y
  +3 % en `box_surface`.

| Bench | main | rama | Diferencia |
|---|---:|---:|---|
| `box_cave_dark` (camino rápido) | 39,3 ns | 41,8 ns | +6,6 %, el ruido de la corrida repetida de main |
| `box_surface` | 46,4 µs | 47,3 µs | +2,1 % |
| `box_far` (el mismo trabajo que `box_surface`) | 48,5 µs | 47,5 µs | sin cambio |
| `box_cave_lamps_rgb` | 322 µs | 306 µs | -3,8 % |
| `box_surface_cold_masks` | 157 µs | 140 µs | -8,7 % |

- Con filtros (solo en la rama): `box_cave_lamps_glass_clear` (75 lámparas encerradas en 525
  vóxeles de vidrio común) 315 µs; `box_cave_lamps_glass_filter` (el mismo vidrio, de color)
  350 µs, +11 %. `box_surface_cold_masks_filters` (la superficie con las tablas de un registro
  con filtros, como el juego) 146 µs, +2 % sobre `box_surface_cold_masks`: el escaneo de paletas.
- Una primera versión hacía la máscara de filtro dentro del pase de las otras máscaras y costaba
  +33 % en `box_surface_cold_masks`. Por eso es un pase aparte.
- `just perf-smoke surface forest` (RTX): superficie 2.136 a 2.140 fps (+0,2 %), bosque 3.894 a
  3.904 (+0,2 %), p99 igual o mejor. Ok. La superficie varía unos ±10 % de una corrida a otra
  con cualquier binario.
- `gpu_light_filter` (prueba nueva, High, Mid y Low de la 780M, la RTX 5050 y llvmpipe, y Low en
  GL de la RTX): detrás del vidrio rojo, 73 % de los píxeles del piso son rojos y ninguno verde o
  azul; detrás del vidrio común, ninguno rojo; sin el muro, las mismas mallas y los mismos píxeles
  con y sin los bloques con filtro en el registro; todos los tiers dan exactamente los mismos
  píxeles que el mejor de su adaptador.

## Consecuencias

- Los vidrios de color tiñen y oscurecen la luz de bloque. Un vidrio oscuro (negro, gris, marrón)
  casi la apaga: un vitral negro delante de una antorcha deja el cuarto en penumbra.
- La tabla usa la curva con gamma 0. Con otro gamma el brillo que pasa se aleja un poco de la
  fracción pedida; el color se mantiene.
- El nivel es entero: cerca de una fuente fuerte el color es fiel, y a pocos vóxeles los canales
  débiles se apagan antes. Un vitral cian lejos de la antorcha se ve verde azulado y después
  azul. Pasa igual con la luz de colores de las lámparas (ADR 0015).
- Los ticks leen la luz del canal más brillante (ADR 0059): un cultivo detrás de un vidrio rojo
  crece con el nivel del rojo, y detrás de un vidrio oscuro puede quedarse sin luz suficiente.
- El cielo no se tiñe. Si algún día se quiere, la opción 2 de arriba es el camino, con su costo.

## Alternativas descartadas

- **Filtro en el shader.** Teñir la luz al dibujar no sabe por qué vidrio pasó: necesita la luz
  por camino, que no existe.
- **Filtro solo como "opacidad por canal".** Es la forma de restar niveles: no respeta el color.
- **Filtro también para el cielo con un tinte por vóxel.** Un índice de tinte por vóxel no se
  combina cuando la luz pasa por dos vidrios ni se mezcla con la de otro camino.
