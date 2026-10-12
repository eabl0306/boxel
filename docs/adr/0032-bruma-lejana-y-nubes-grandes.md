# ADR 0032: bruma lejana y nubes grandes sobre el LOD

- Estado: borrador (issue #47), a confirmar con el vuelo del dueño
- Fecha: 2026-10-11
- Enmienda: ADR 0030 (el límite de distancia, la cola pasado el tope) y ADR 0031 (decisión 6)

## Contexto

Con el LOD lejano (ADR 0031) el terreno llega a 12.288 bloques, pero las capas de `FogPlugin`
(ADR 0030) terminan donde terminan sus celdas: el valle a 384 bloques y las nubes a 768. Desde
una altura se ve una burbuja de niebla alrededor de la cámara y, más allá, todo limpio. El
dueño pidió tres cosas:

- que la niebla se vea bien con el LOD, sin burbuja;
- nubes grandes y lejanas (cubos que llenen de 200 a 300 de altura) para comparar en vuelo;
- después, una ronda de optimización de la niebla (M2, fuera de este ADR hasta el vuelo).

Caminar celdas hasta el horizonte no entra: a 12 km un cubo de 2 bloques es mucho menos que un
píxel y el presupuesto de 64 pasos se acaba mucho antes. Hace falta otra cosa pasado cierto punto.

## Decisión

### 1. Una capa sigue como bruma pasadas sus celdas

`FogLayer` tiene un campo nuevo, `haze`, de 0 a 1 (0 por defecto). Una capa tiene ahora dos
distancias:

- **Fin de las celdas** (`max_distance`, mismo significado que antes): hasta ahí se recorren los
  cubos.
- **Alcance**: con `haze` en 0 es el mismo fin de las celdas (la capa de siempre). Con `haze`
  mayor que 0, la capa sigue como una bruma suave de `haze` veces su extinción media a cada
  altura, hasta el final de su desvanecido.

Con `haze` en 0 la imagen es la de antes, salvo en los rayos que agotan el presupuesto de pasos
(sección 4). Todos los presets de hoy (`valley`, `clouds`, `both`, `inside`, `sunset`) quedan en
0, así que `both` sigue siendo la referencia de las mediciones.

### 2. La bruma es un perfil por altura

En la CPU, cada vez que cambian los ajustes, se arma un perfil de 8 nudos de extinción media
(`HazeProfile`, `haze_profile`). Los nudos cubren solo las filas que pueden tener niebla: la
fila pegada a un plano con borde siempre está vacía y queda afuera. El valle va de 48 a 68, así
que no hay bruma bajo el mar (47). Cada nudo sale del ruido entero de esa fila, con los bordes y
la subida del umbral incluidos, sobre una muestra fija de 1024 celdas: determinista e igual en
cualquier origen. Entre nudos la extinción es lineal.

Las nubes con subida salen más densas en la base que en el tope, cosa que una sola media no
podía dar (habría un escalón donde terminan las celdas).

### 3. La integral es exacta por tramos

Un rayo de bruma se corta donde cruza la altura de un nudo y donde empieza el desvanecido: a lo
sumo 10 tramos, de 1 a 3 en los rayos rasantes. En cada tramo la extinción es lineal en la
distancia, así que su profundidad óptica es exacta, y también la de cada profundidad de píxel que
cae adentro (`integrate_ramp` en el shader, `State::segment_ramp` en la CPU). Las colinas
lejanas que asoman en la bruma tienen la transmitancia justa. No hay restas de primitivas, así
que tampoco cancelación en `f32`.

El color de cada tramo se toma en el punto de donde viene su luz en promedio, no en el medio: un
tramo largo y lejano emite casi todo en su frente. Para una extinción constante es exacto.

### 4. La costura y la cola

- Las celdas cuyo medio cae en el último 25 % antes del fin de las celdas se funden con la bruma
  (`FAR_BLEND`), como la niebla suave se funde con las celdas cerca de la cámara. Al fin de las
  celdas, una celda vale lo mismo que la bruma. El salto de tramos vacíos nunca entra en ese
  anillo: ahí una celda vacía también lleva bruma.
- Una capa con todas las celdas llenas, que la marcha toma entera sin recorrer celdas, también se
  funde: su cuerpo termina donde empieza el anillo y el anillo va en 8 trozos que se funden como
  celdas (densidad y sombra de cara). Sin eso quedaba un círculo donde terminan sus celdas si su
  bruma es menor que 1 o si sombrea por caras.
- Un rayo que empieza pasado el fin de las celdas no recorre ninguna: solo bruma, sin pasos.
- Pasado el presupuesto de pasos, el resto de las celdas usa el mismo perfil (antes, 8 trozos de
  la media con `MeanCoverage`, que ya no existe). Cambia un poco la imagen de los rayos que
  agotan el presupuesto, también en los presets de hoy, y es más barato.

### 5. `Scene` sigue al LOD solo con bruma

Una capa `FogFade::Scene` con `haze` mayor que 0 toma el desvanecido del terreno más lejano que
se dibuja: el del LOD en su horizonte mientras dibuja (`LodPass::fog_params`, de 12.224 a 12.288
con `far`), si no el de la vista. Su bruma termina ahí, con el terreno. Con la niebla de
distancia apagada termina de golpe en el mismo lugar, como el terreno.

Una capa sin bruma conserva la regla de antes: su alcance y su desvanecido no cambian con el LOD
(ADR 0031, decisión 6, enmendada así).

El renderer pasa el desvanecido del LOD a la pasada de niebla en una sola llamada, dentro de la
rama de la niebla: sin `FogPlugin` no se calcula nada.

### 6. Límites

- `max_distance` es como mucho 4096 de las celdas más chicas de la capa y 65.536 bloques
  (`MAX_FOG_CELLS`, `MAX_FOG_DISTANCE`). Las celdas recorridas siguen a menos de 4096 celdas de
  la celda del origen, así que los enteros del ruido conservan su margen. Cubos de 2 bloques
  llegan a 8.192, de 16 a 65.536.
- La bruma usa solo alturas relativas al origen de render y largos sobre el rayo, nunca enteros
  de celda: lejos del origen da lo mismo.
- El uniform `Fog` pasa de 1008 a 1136 bytes (dos `vec4` más por capa, los nudos). Ningún
  binding, textura ni sampler nuevo. Los tres tiers dan la misma imagen.

### 7. Nubes grandes con los campos de siempre

Los presets nuevos de `boxel_demo::fog` usan solo campos que ya existían. Toman el horizonte del
LOD (768 bloques sin LOD, donde terminan las nubes de hoy):

| preset | cubo | banda | cubos en | ruido | densidad | celdas hasta |
|---|---|---|---|---|---:|---:|
| `cumulus16` | 16 | 176 a 320 | 208 a 288 | escala 8, 2 octavas | 0,18 | 3.072 |
| `cumulus20` | 20 | 160 a 340 | 200 a 300 | escala 8, 2 octavas | 0,15 | 3.840 |
| `cumulus24` | 24 | 144 a 336 | 192 a 288 | escala 8, 1 octava | 0,12 | 4.608 |
| `high_deck` | 32 x 8 | 400 a 440 | 408 a 432 | plano, escala 16 | 0,05 | 1.536 |

- Todas: ruido 3D (salvo la capa alta), suavidad 0,25, base gris, suaves a 32 bloques, bruma 1,
  desvanecido de la mitad del horizonte al horizonte. `decks` es `cumulus16` con `high_deck`.
- Los cúmulos llenan lo que pidió el dueño (cubos de 200 a 300): bordes de 2 filas abajo y
  arriba (la fila del plano siempre vacía, la siguiente subida a la mitad) y una subida suave del
  umbral (0,15), así cada fila del rango tiene al menos un cuarto de la cobertura de la más densa
  y la base es más ancha que el tope. Con los valores del plan (subida 0,25, borde de 3 filas
  arriba) solo se llenaban 2 filas, unos 32 bloques. Lo comprueba un test sobre el perfil de
  bruma.
- Un cubo cruzado de cara a cara es cerca de 95 % opaco: un rayo que encuentra una nube sale tras
  2 a 4 cubos llenos, a cualquier distancia. Lo barato viene de la densidad, no de saltar.
- Las celdas terminan a 192 celdas (contadas en su lado más chico), donde una celda mide unos 4
  píxeles a 1080p; más lejos solo titilaría con el viento. La capa alta usa sus filas de 8: con
  4.096 bloques sus filas eran más finas que un texel y se veían como puntos.
- `hazy_valley()` es `valley()` con bruma 1. Cada preset nuevo de los ejemplos es el valle con
  bruma más sus nubes.

### 8. Ejemplos

- `--fog` suma `cumulus16`, `cumulus20`, `cumulus24` y `decks`, después de `both`. F7 los recorre
  en ese orden.
- `--fog-haze F` (0 a 1, 1 por defecto) multiplica la bruma de cada capa. En 0 las nubes se
  desvanecen en sus celdas (de la mitad al final) en lugar de cortarse en un círculo: es la
  burbuja de hoy. Shift+F7 lo recorre: 1, 0,25 y 0.
- Las nubes lejanas toman el horizonte del LOD mientras dibuja. Si no dibuja nada (oculto con
  F8, un adaptador por software, un backend GL, una pasada que falló o su guarda, según
  `LodStats::off`) terminan a 768, como sin LOD. Cuando eso cambia, con F8 o sin tocar nada, las
  capas se rearman.
- `stress` y `snapshot` pasan el horizonte del preset de `--lod` con la misma regla y aceptan
  `--fog-haze`.

## Límites conocidos

- **Bruma más densa que la realidad vista desde arriba.** `e` a la menos la profundidad media es
  menos que la transmitancia media entre huecos, así que desde un avión los bancos lejanos se
  ven como un velo donde había claros. Perillas: bruma menor que 1, o celdas del valle más
  lejos.
- **Un mar de niebla.** Con el valle en 1, el mar y los bajos lejanos se ven blancos hasta el
  horizonte, con las colinas asomando. Es la idea aceptada; Shift+F7 muestra 0,25 y 0.
- **Franja de cielo bajo el banco de nubes.** Mirando al horizonte desde y 140, entre el borde
  del LOD y la base del banco de nubes queda una línea de cielo de unos 10 píxeles a 1080p: los
  rayos que entrarían a la banda más allá del horizonte ya tomaron el color del cielo.
- **Cubos grandes cerca del fin de las celdas** titilan un poco con el viento (unos 2 texels de
  media resolución a 1080p). El anillo de fusión y la suavidad lo suavizan.
- **Rayos que agotan el presupuesto con bruma menor que 1** toman el perfil completo hasta el fin
  de las celdas y la fracción después: un salto de densidad sobre ese rayo. Solo pasa con 0,25 y
  esos rayos suelen estar casi opacos ahí.
- **En GL (Low) no hay LOD**, así que una capa `Scene` con bruma solo la muestra cuando la vista
  pasa de sus celdas.

## Alternativas descartadas

- **Un límite de 2^16 celdas.** 4096 celdas ya llegan a 65 km con cubos de 16.
- **La primitiva cerrada del perfil** con un plan B contra la cancelación: cortar en los nudos es
  exacto y más simple.
- **Una sola media por capa.** Deja un escalón en la costura de las nubes con subida.
- **Pendientes hasta después del vuelo:** la marcha lejana suave anclada al mundo para las nubes,
  un mapa de cobertura (grupos de nubes con cielo abierto entre ellos) y un presupuesto de pasos
  por capa.

## Cómo se probó

- `boxel_fog`: rangos de `haze` y del límite en celdas; el perfil (filas que cubre, sin borde,
  subida, determinista); la bruma contra una integral fina (error relativo menor que 1e-3 en
  rayos empinados, rasantes y horizontales); profundidades de píxel dentro de un tramo; la
  costura continua, también en una capa llena; ningún salto dentro del anillo; rayos solo de bruma sin pasos; una capa sin
  bruma da bit a bit lo de antes (hash fijo tomado en 0bd2849).
- `boxel_render`: el desvanecido de una capa `Scene` con bruma con y sin LOD; escenas nuevas de
  referencia en los tres tiers (bruma lejana, bruma `Scene`, presupuesto agotado, una capa llena
  con bruma a la mitad y sombra de caras desde abajo y desde arriba, una capa de cubos de 16 desde
  abajo, desde adentro y desde arriba); el ruido contra el oráculo cerca de
  ±4096 celdas; `gpu_lod` comprueba que el valle con bruma llega al horizonte del LOD.
- `boxel_demo`: presets válidos, suaves, sin planos y sin saltos por tick; las nubes densas; la
  tabla de pasos (abajo) con techos fijos.
- Ejemplos: nombres, horizontes, `--fog-haze`, Shift+F7 y F8.

**Pasos por rayo** (`cargo test -p boxel_demo fog_steps_per_ray -- --nocapture`): la marcha de
referencia sobre un abanico fijo de rayos, con el horizonte `far` y una vista de 512. Poses:
`stress` (y 140, 20 grados abajo), `horizon` (y 140, 8 arriba), `above` (y 420, 25 abajo) y
`deck` (y 250, nivel).

| preset | pose | pasos medios | p90 | tope agotado | trozos suaves | trozos de bruma |
|---|---|---:|---:|---:|---:|---:|
| both | stress | 16,2 | 46 | 2 % | 0,0 | 0,0 |
| both | horizon | 20,9 | 46 | 2 % | 0,0 | 0,0 |
| cumulus16 | stress | 19,5 | 64 | 10 % | 0,0 | 0,4 |
| cumulus16 | horizon | 24,8 | 64 | 10 % | 0,3 | 0,4 |
| cumulus16 | above | 25,1 | 63 | 9 % | 0,0 | 1,5 |
| cumulus16 | deck | 11,3 | 20 | 0 % | 53,0 | 0,0 |
| cumulus20 | horizon | 25,8 | 53 | 7 % | 6,7 | 0,4 |
| cumulus24 | stress | 16,3 | 46 | 5 % | 7,2 | 0,3 |
| cumulus24 | horizon | 22,0 | 46 | 5 % | 22,4 | 0,3 |
| decks | horizon | 27,5 | 64 | 12 % | 0,3 | 0,9 |
| decks | above | 32,6 | 64 | 11 % | 40,1 | 1,6 |
| inside | stress | 64,0 | 64 | 100 % | 27,3 | 1,6 |

Los cúmulos dan unos pasos más que `both` mirando al horizonte; `cumulus24` da menos pasos. Para
llenar de 200 a 300, sus bandas empiezan en 176, 160 y 144, cerca de las poses a y 140: los rayos
que suben desde ahí entran en la niebla suave de `cumulus20` y `cumulus24` (6,7 y 22,4 trozos
suaves por rayo mirando al horizonte), aunque las primeras filas de la banda están vacías. Volar
dentro de una nube cuesta trozos suaves, como `inside`.

**Imágenes** en `bench-results/fog-far/look/` (fuera de git, con `comandos.sh` y `leeme.txt`):
cada preset desde la pose del horizonte, `cumulus16` desde adentro y desde arriba, y el valle con
bruma 1, 0,25 y 0 contra `both`. Las de los cúmulos tras el ajuste de bandas (02 a 07, mismas
poses) están en `bench-results/fog-far/look-ajuste/`.

**Costo.** La línea base (S0, 0bd2849, RTX, 1280x720, radio 16, sin tope): `--fog both` cuesta
de 0,97 a 1,06 ms en la mediana. Las mediciones de este ADR (S1 a S3) se corren cuando el M1
esté completo y van aquí y en `docs/rendimiento/`.

## Pendiente

- El vuelo del dueño (`voxel_world --lod far --fog both --camera 0,140,0,45,8`): F7 por los
  cuatro presets nuevos, Shift+F7 en cada uno, subir a y 420, F8 por los horizontes.
- Decisiones del dueño tras el vuelo: fuerza y alcance de la bruma del valle (1 y 384 por ahora),
  bruma o marcha lejana para las nubes, mapa de cobertura, qué presets quedan y la tecla del
  factor (Shift+F7 o F11).
- M2: la ronda de optimización de la marcha, medida contra `both`.

## Consecuencias

- Un juego hace que una capa llegue al horizonte con un solo campo, sin cambiar el resto.
- El layout de `FogLayerGpu` cambia y `MeanCoverage` desaparece: un consumidor fuera del repo que
  los use se rompe (por eso el commit del M1a es `feat(fog)!`).
- `boxel_fog::march::MarchResult` cuenta trozos suaves, trozos de bruma y si se agotó el
  presupuesto, para la tabla de pasos.
