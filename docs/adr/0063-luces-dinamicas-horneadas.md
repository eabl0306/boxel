# ADR 0063: luces dinámicas horneadas cerca de la luz

- Estado: aceptado (issue #142). El dueño aprobó tocar el streaming, el render y los shaders el
  2026-11-02.
- Fecha: 2026-11-02

## Contexto

Las luces dinámicas del ADR 0062 se aplican en el shader. Tienen dos fallas:

- **Sin AO bajo la luz.** El mallador no pone AO en las caras con las 4 esquinas a oscuras
  (`AoPolicy::BakedLitOnly`). La antorcha en la mano ilumina esas caras y se ven planas.
  `AoPolicy::Baked` en todo el mundo lo arregla, pero cuesta de 8 a 16 % de fps en el juego y de
  17 a 31 % en el motor, con 69 a 85 % más quads.
- **Atraviesan paredes.** El shader ilumina toda cara que mira a la luz y está a su alcance, aunque
  haya un muro en medio.

El dueño aprobó tratar aparte los chunks cerca de una luz: AO en sus caras oscuras y una luz que
respeta los muros. Los demás quedan como hoy, y sin luces todo queda igual que en main.

Lo que pesa:

- **Low** tiene un solo uniform block y un solo par de textura y sampler (ADR 0011). Una segunda
  textura cae en la unidad 0 en el GL de NVIDIA, sin error.
- Los tres tiers dan los mismos píxeles.
- Los ticks leen la luz guardada de los bloques (ADR 0059). Las mallas cerca de una luz no deben
  cambiar lo que leen.

## Decisión

### La luz se hornea como una luz de bloque

Es lo que hacen los mods de Minecraft (LambDynamicLights y otros).

- `DynamicLight` pasa a `boxel_world` (`boxel_render` lo reexporta: el juego no cambia nada).
- El mundo hornea las `WorldSettings::baked_lights` luces más cercanas a un viewer (8 por defecto;
  0 deja todas en el shader). `BakedLights` dice cuáles son y cuánto costaron.
- Cada luz brilla desde un vóxel (su posición, con 1/8 de bloque de histéresis en el borde).
- Cada job de malla cuyo chunk acolchado puede alcanzar una luz inunda esa luz por los bloques de
  su caja (`boxel_light::add_point_lights`) con las reglas de la luz de bloque: opacidad 15 frena,
  entrar cuesta `max(1, opacidad)`, los filtros tiñen por canal. La suma a la luz del chunk por el
  máximo de cada canal. La regla es monótona y se mezcla por máximo, así que el resultado es, bit a
  bit, el de un emisor puesto en ese vóxel. Rodea muros como una antorcha puesta y nunca los cruza.
- La luz que el job guarda para el juego es la de los bloques solos. Los ticks no ven la luz
  dinámica.

### El AO sale solo

Las caras que la luz ilumina ya no están a oscuras en la luz con la que se malla, así que
`BakedLitOnly` les pone AO, igual que bajo una antorcha puesta. El juego sigue con `BakedLitOnly` y
no hace falta otra política por chunk. Las caras fuera del alcance siguen a oscuras y sin AO, como
hoy.

### Cuándo se remalla

- Cuando una luz pasa a otro vóxel, cambia su emisión, aparece o se va, se remallan los chunks que
  alcanzaba y los que alcanza ahora (hasta 8 por posición, casi siempre los mismos; unos 7 por
  paso medidos), como un lote de edición: se publican juntos.
- Si su lote anterior todavía no salió, la luz espera en su vóxel hasta que salga y se mueve en el
  frame siguiente. Así una luz más rápida que las mallas igual llega.
- Una edición dentro del alcance de una luz que cambia opacidad o filtro remalla los chunks de esa
  luz: la luz guardada a su alrededor puede estar a oscuras.
- Una luz quieta no remalla nada.

### El render

- El shader del ADR 0062 sigue para las luces que el mundo no hornea (más de 8, o un mundo sin
  luz). Las horneadas no van al shader.
- Los modelos toman todas las luces en su sonda, como en el ADR 0062.
- No hay cambio en los shaders ni en el uniform.

## Opciones descartadas

- **`Baked` en todo el mundo** (la rama `dynlights-ao` del juego): arregla el AO pero no los muros
  y cuesta de 8 a 16 % de fps.
- **Luz en el shader y la inundación como máscara de oclusión en un volumen por luz en la GPU.**
  Mantiene el movimiento suave. En Low no hay lugar: no admite una segunda textura (ADR 0011) y el
  uniform tiene unos 6,7 KiB libres. Un volumen de 1 bit por vóxel del octaedro de alcance ocupa
  752 bytes por luz, así que entran 8 luces y solo con sombra dura, sin la luz que rodea esquinas.
  Además los chunks cerca de la luz se remallan igual para el AO.
- **La máscara de oclusión en las mallas.** El quad no tiene bits libres, y se remalla en cada
  vóxel igual que horneando la luz entera.
- **Un mapa de sombras por luz.** Seis pasadas por luz y otra regla que la de una antorcha puesta.

## Aproximaciones

- La luz salta de vóxel en vóxel, como en LambDynamicLights, y llega con la latencia de un lote
  de edición (abajo).
- Un modelo toma la luz sin muros (en su sonda, ADR 0062).
- Las celdas que se mueven (ADR 0054) no toman las luces horneadas.
- Más de `baked_lights` luces: las demás van al shader, con sus fallas.

## Costos

Medido en la RTX 5050 (radio 16, sin tope) y en la 780M, con `just perf-smoke`.

**Sin luces**, contra e5a796e: el mismo mundo (los mismos quads) y por eso los mismos píxeles,
porque ningún shader ni uniform cambió. Las pruebas de GPU pasan sin cambios.

| Caso | fps | p99 ms |
|---|---:|---:|
| superficie | 2010 a 1943 (-3,3 %, ruido de este caso) | 1,15 a 1,38 |
| cueva quieta | 2318 a 2408 (+3,9 %) | 0,92 a 0,89 |

**Una antorcha** (`stress --held-light 14,11,6`), contra el mismo binario sin ella:

| Caso | RTX High | RTX Mid | RTX Low | 780M High |
|---|---:|---:|---:|---:|
| cueva quieta | -0,2 % | -1,4 % | +1,1 % | -1,3 % |
| cueva, ruta a 8 bloques/s | -0,9 % | -2,2 % | +0,5 % | -11,7 % |
| superficie quieta | -0,3 % | | | |

Contra la antorcha en el shader (ADR 0062, `--baked-lights 0`), horneada da +2,6 % (cueva
quieta) y +3,1 % (ruta) en la RTX: el fragmento ya no recorre luces.

**El juego** (`just perf-smoke`, RTX), contra su main sobre e5a796e, sin antorcha: superficie
-0,5 %, subterráneo +3,3 %, costa +4,3 %, río +0,3 %. Con la antorcha en la mano
(`--give boxelcraft:torch`, el mismo binario): +0,5 %, -1,2 %, +0,4 % y -1,7 %, todo ruido. Con
`Baked` en todo el mundo eran -8 a -16 %.

**Remallado.** En la ruta a 8 bloques/s, 8 pasos de vóxel por segundo y 56 chunks remallados por
segundo (unos 7 por paso). Caminando (4,3 bloques/s) son unos 30 por segundo y corriendo (5,6)
unos 40. Quieta, 0. Cada job cerca de la luz suma su inundación: 7 µs en una cueva y 121 µs en
aire abierto (`light/point_cave`, `light/point_surface`), contra 48 a 335 µs de la luz del chunk.

**Latencia.** La de un lote de edición: 2 frames a 144 fps (unos 14 ms) y 3 o 4 sin tope (unos
2 ms), medido en `docs/rendimiento/lotes-de-edicion-2026-11.md`.

**Memoria.** Las mallas cerca de la luz son las de una antorcha puesta: en la cueva quieta,
8 926 346 a 8 927 042 quads (+0,01 %), contra +85 % con `Baked` en todo. Cada hilo de jobs guarda
72 KiB de inundación y una copia de la luz acolchada (78 KiB) la primera vez que hornea una luz.

**Tiers.** Nada cambia en la GPU: los tres tiers dibujan las mismas mallas con el mismo shader.
La prueba `gpu_dynamic_light_walls` da los mismos píxeles en High, Mid y Low (RTX en Vulkan y
GL, 780M, llvmpipe).

**La 780M al moverse.** En la ruta pierde 11,7 % (repetido: 11,6 %). No depende de cuántos
chunks se remallan: una luz de alcance 5 remalla 18 por segundo y pierde 10,7 %, y una de
alcance 0 (9,6 por segundo, mallas iguales que no se suben) no pierde nada. Lo caro parece ser
subir mallas nuevas de chunks visibles cerca de la cámara, no la luz: debería pasar igual con
cualquier remallado así (poner y sacar antorchas). No lo medí ni lo investigué: es del render.

## Consecuencias

- Una luz que se mueve remalla hasta 8 chunks por vóxel que cruza. Quieta no cuesta nada.
- En la 780M, moverse con una luz en una cueva cuesta unos 12 % de fps (arriba). Lo decide el
  dueño: aceptarlo, bajar `baked_lights` en esa GPU, o mirar por qué cuesta subir mallas visibles.
- Las mallas cerca de la luz son las de una antorcha puesta: los mismos quads, la misma memoria.
- Sin luces, ninguna malla cambia y ningún píxel cambia.
