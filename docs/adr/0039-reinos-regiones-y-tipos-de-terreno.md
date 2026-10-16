# ADR 0039: reinos, regiones y tipos de terreno

- Estado: aceptado (issue #117). El dueño aprobó el plan de generación v2 el 2026-10-15, con el
  diseño B como base y todas las recomendaciones del plan. Es un contrato. Lo construyen los
  pasos de la sección 20, cada uno en su rama, y cada parte protegida pide su propio OK cuando le
  toca. Hechos: WG1, WG-REG y WG-REGION (#119); donde el código precisó el contrato, este texto
  ya lo dice.
- Fecha: 2026-10-15
- Revisa: ADR 0024 sección 4 y ADR 0029 sección 7 (una clave de metadatos reservada más,
  `boxel:worldgen`).
- Se apoya en: ADR 0014 (coordenadas `i64`), ADR 0031 (lejanía) y ADR 0034 (ciclo de vida del
  mundo). Marca la dirección para ADR 0015 sección 4 (el piso de la caminata del cielo), que solo
  cambia con el OK de su item.

## Contexto

El contrato de hoy es `WorldGenerator` (`boxel_worldgen`):

- `generate(key, registry)` es pura, tarda un tiempo acotado a cualquier altura y devuelve
  `Uniform` donde no hay nada que poner.
- `vertical_chunks()` es un solo rango de capas de chunks para todo el mundo. Por defecto no
  promete nada.
- `lod()` da la fuente de la lejanía, si la hay (ADR 0031).
- `name()` va a los logs y se guarda en `boxel:generator`.

No hay versión, ni cota por columna, ni reinos, ni consultas (la altura, el bioma o el reino de un
punto).

El juego pide más: varios tipos de generación por dimensión y por bioma, extensibles por mods. Y
el dueño quiere un mundo vertical (2026-10-15): cavando hondo se llega a un Nether; hay un agujero
gigante con islas flotantes, agujas de 12 km, un techo del cielo con cuevas y ciudades, un bioma al
revés en la cara de abajo de ese techo que toca las agujas, una torre con escalera en espiral y, al
final, gravedad invertida.

Con el contrato de hoy eso no entra:

- Un solo rango vertical alimenta el aire conocido del streaming, el comienzo y el piso de la
  caminata del cielo, y el piso y el techo de la lejanía. Un tipo alto en un solo lugar los sube
  en todo el mundo. Un reino arriba de la superficie rompe la caminata: o todo queda oscuro, o el
  rango queda sin límite y `SkyRule::Open` ilumina todas las cuevas.
- Hay una sola dimensión cargada a la vez (`ChunkMap` indexa por `ChunkPos`), y cambiar es cerrar
  y abrir (ADR 0034). Los ticks y el reloj no llevan dimensión.
- El store rechaza cualquier `FORMAT_VERSION` que no sea la actual y no tiene lector de versiones
  viejas. Un `boxel:generator` distinto solo avisa.

El plan completo está en `.claude/plans/worldgen-plan-v2.md` (local, no se sube). Junta el diseño
B (la base), injertos del diseño A, las correcciones de dos jueces y una revisión adversarial. Este
ADR fija la parte del motor. El contenido (campos, tipos, biomas, números) es del juego; los
ejemplos de acá usan nombres `game:`.

## Decisión

### 1. Capas, y qué es del motor y qué del juego

```
WorldPreset (game:default, game:large_biomes, ...)  se elige al crear el mundo, se guarda por id
 `- dimensión -> DimensionDef + spec del generador  una dimensión cargada a la vez
    `- DimensionGenerator: RealmStack + un generador  un generador por dimensión, despacha por reino
       por reino (RealmStack: los reinos ordenados por y)
       `- reino = RealmSpec + RegionGenerator<Cx>   su Context, su mapa de regiones, sus tipos,
                                                    su decoración y su entorno
```

- El motor (`boxel_worldgen`, no protegido) pone el mecanismo: ids y registros, `RealmStack` y
  `RealmSpec`, el selector y la mezcla, `RegionGenerator`, la pasada genérica de celdas de la
  lejanía, presets y tablas de dimensiones, el registro en el guardado, el lado de consultas y los
  harness de tests.
- El juego pone todo el contenido: cada campo, tipo, regla, bioma, número, reino y preset. En los
  docs del motor los ejemplos se llaman `game:`, y en sus tests `test:` (`tools/no_content.py`).
- `WorldGenerator` y `LodSource` conservan sus firmas. Los métodos nuevos tienen valor por defecto
  y dejan todo como hoy:
  - `version() -> u32`: 0.
  - `realms() -> &RealmStack`: un solo reino que abarca toda y, con cielo `Open` y `lod = Draw`
    (el único reino `Draw` que pide la sección 2).
  - `as_query() -> Option<&dyn WorldQuery>`: `None`.
  - más adelante, `column_bounds` (WG4, sección 14).

  `LodSource::version` devuelve `WorldGenerator::version`.
- Dimensiones: `DimensionDef { id: GenId, factory, realms }`. Un preset le asigna a cada id de
  dimensión un spec de generador: el id de un tipo de generador y sus parámetros tipados.
- Fábricas, no instancias: la app guarda `GenId -> fn(seed, params, &Frozen) -> Arc<dyn
  WorldGenerator>`, así un mundo se reabre desde sus metadatos. El juego arma un generador nuevo en
  cada apertura.
- Una dimensión cargada a la vez, como hoy. El juego usa una sola (`game:overworld`) y su Nether es
  un reino dentro de ella. Una segunda dimensión (portales) pediría ticks y reloj por dimensión, o
  sea cambiar el formato con un lector de la versión 1 (la de hoy). Nada de este ADR depende de eso.

### 2. Reinos apilados en y

Un reino es una losa horizontal de la dimensión entre dos planos, `y_lo <= y < y_hi`. `y_hi` puede
ser +infinito e `y_lo`, -infinito.

- Los planos están en múltiplos de 4096 bloques y dentro del mundo (+-`MAX_BLOCK_COORD`). Así
  ningún nodo de la lejanía hasta el nivel 7 (4096 bloques) cruza un plano, y un chunk está
  siempre en un solo reino.
- Los reinos de una dimensión cubren cada y exactamente una vez: sin huecos ni solapamientos.
- `realm_of(y)` es una búsqueda binaria entre a lo sumo 8 planos: O(1) para quien pregunte
  (streaming, lejanía, luz, física, la cámara).
- Despacho: `generate(key)` llama a `realm_of` con un vóxel de su chunk (`key.pos.y << 5`: la clave
  da la capa en chunks y `realm_of` toma vóxeles) y al generador de ese reino. Es una llamada
  virtual por chunk.
- Cada reino tiene su sal (`salt(id del reino)`, mezclada en la semilla), así agregar un reino no
  mueve el azar de otro.
- `vertical_chunks()` sigue siendo la unión de los límites de todos los reinos, para los lectores
  de hoy y el respaldo de la lejanía. Los lectores por reino llegan con WG4, EW-REALMSKY y
  EW-SKYUNDER (secciones 14 y 15). Con un solo reino la unión es el rango de hoy y nada cambia en
  el streaming, la luz ni la lejanía.

`RealmSpec { span, form, datum, fluid, floor, sky, lod, env, version }`, registrado bajo su id:

- `span`: los dos planos.
- `form`, la forma del reino. Los tipos de un reino dan las superficies que pide su forma, y la
  mezcla corre por superficie.
  - `Ground`: sólido bajo una superficie H (la superficie del mundo).
  - `Cavern`: sólido bajo un piso F y sobre un techo R, con F < R (el Nether del juego).
  - `Ceiling`: sólido sobre una cara de abajo U (el techo del cielo).
  - `Void`: aire, solo con tipos de volumen (islas del cielo, algo como el End).

  WG-REGION construye solo `Ground`. `Cavern` llega con WG-CAVERN (V1), `Ceiling` con V4 y `Void`
  cuando un reino lo pida. Hasta entonces el chequeo al congelar rechaza una forma que no está
  hecha.
- `datum`: el origen de alturas del reino (el nivel del mar en la superficie, el mar de lava en el
  Nether, la base de la cara de abajo en el techo). Los tipos trabajan en `HeightQ = i32`, en 1/256
  de bloque y relativo al datum: +-2^23 bloques alrededor de él, mucho más que cualquier relieve de
  acá. La altura en bloques es `datum + (h >> 8)`, con desplazamiento aritmético.
- `fluid`: una `FluidRule` por reino, local a la columna: `SeaLevel { level, fluid }` o `None`.
  - Los tipos nunca ponen fluido, así un borde entre tipos nunca hace una pared de agua.
  - `SeaLevel` llena una columna desde su suelo hasta `level` solo si la altura de la columna
    antes de los términos de núcleo (sección 7) queda bajo `level`. Así el núcleo que hunde un
    agujero gigante queda seco y no se vuelve un lago de 4 km. Su regla del selector lo pone solo
    tierra adentro, así su borde es tierra seca.
- `floor`: el estado bajo la superficie más baja del reino, que la lejanía usa como `below` (la
  piedra, la roca del Nether), o `AIR` en las formas `Ceiling` y `Void`. Hay uno solo por reino
  porque `below` tiene que coincidir entre columnas vecinas (contrato de `LodColumn`).
- `sky`: `Open`, `Under` o `Closed` (sección 15).
- `lod`: `Draw` u `Off` (sección 10). Exactamente un reino `Draw` por dimensión.
- `env`: un `GenId` de la tabla de entornos del juego (sección 16).
- `version`: `u32`, como toda entrada (sección 11).

### 3. Transiciones entre reinos

El plano es un corte duro. La continuidad a través de él la acuerda el contenido; nunca se mezcla.

- **Roca con roca** (el basamento de la superficie sobre el techo del Nether): el relleno de cada
  reino pasa a una roca de frontera común en sus últimos 256 bloques. Hay sólido de los dos lados
  y el plano no se ve.
- **Aire con sólido** (el cielo de la superficie bajo la cara de abajo del techo): el plano queda
  en el aire, y las superficies del techo, arriba de él.
- **Contactos** (una aguja que llega al techo, la torre, un agujero que rompe hacia el Nether): un
  `ContactField`, un campo celular disperso en el espacio de semilla compartido de la dimensión
  (un sitio por celda de 2^16 bloques, con una compuerta por hash), le da a cada contacto un centro
  y un radio. Los dos reinos leen la misma función entera y mantienen su lado sólido (o abierto)
  dentro de la huella en el plano, así se tocan sin costura. El `ContactField` llega con V3,
  porque las agujas se apoyan en él.
- Los tipos que tocan un plano se ubican desde él. La distancia del `ContactField` es un parámetro
  del Context, y la aguja, el agujero que rompe y la montaña invertida son reglas `Point` sobre esa
  distancia (un disco alrededor del sitio; la sección 7 dice qué radio pide en cada distancia).
  Ningún reino lee el selector del otro.

Ejemplo: el reparto que el dueño eligió para el juego (decisión 5 del plan).

```
+infinito  +
           | reino del techo del cielo (Ceiling, sky Under, lod Off): sólido hacia arriba
           | para siempre, con cuevas y ciudades a toda altura
  14336    | datum de la cara de abajo U: relieve colgante, montañas invertidas hasta el plano;
           | el aire bajo ella se ilumina desde abajo
  12288 ---+ plano (el horizonte del preset far; el techo nunca está en la lejanía)
           | reino de la superficie (Ground, sky Open, lod Draw): terreno de SEA - 180 a
           | SEA + 400, agujas hasta el plano, el agujero hacia abajo
     63    | nivel del mar (SEA)
  -4096 ---+ plano (la roca pasa a la del Nether en los últimos 256 bloques sobre él)
           | reino del Nether (Cavern, sky Closed, lod Off): techo cerca de -4400, mar de
           | lava en -4800, piso cerca de -4900, y basamento sólido hacia abajo para siempre
  -8192 ---+ plano libre: un reino futuro o de un mod puede cortar ahí el basamento del Nether
-infinito  +
```

Unos 4,3 km de cavar llegan al Nether, y 12 km de subir llegan al techo (por una aguja o por la
torre). Cavando desde la superficie se cruza el plano de -4096 sin verlo: roca sólida hasta el
techo de la caverna. La altura de la caverna (unos 400 bloques) es un número de ajuste para las
imágenes de V1.

### 4. El mapa de regiones

- Una retícula de paso `Ls = 2^ls`, 256 por defecto. Cada punto `q` de la retícula recibe un tipo
  de terreno.
- Las reglas son datos del juego, y los mods les agregan entradas: `SelectorRule { realm,
  priority, condition, scope, entries, version }`, registrada bajo su id. Cada regla nombra el
  reino de su mapa, así un mod agrega una regla a un reino sin tocarlo. Una entrada agregada a una
  regla que ya existe es un `RuleEntry { rule, entry, version }` en su propio registro (un valor
  registrado no se puede ampliar). `condition` es una conjunción de intervalos enteros sobre
  parámetros con nombre del Context (ids que el Context declara, leídos como `i32` en los puntos
  de la retícula).
  Decide la primera regla cuya condición se cumple, en orden `(priority, id)` (sección 11: la
  prioridad más alta primero). Todo mapa tiene una regla de reserva, de prioridad mínima, con
  alcance `Cell` y sin condición, así cada punto recibe un tipo; el chequeo al congelar rechaza un
  mapa sin ella.
- Alcances (`scope`):
  - `Cell(cs)`: el tipo de la celda de región. Es un campo celular con jitter, de paso 2^cs (2^12
    por defecto, nunca menos que Ls) y deformado por la deformación grande del Context. Cada punto
    va al sitio más cercano, exacto (`Cellular::nearest` de WG1). Las celdas salen de la semilla
    del reino, así las reglas `Cell` de un tamaño leen las mismas celdas. Da regiones contiguas de
    2 a 6 km.
  - `Point`: un contorno de un campo, muestreado cada Ls (por ejemplo, cordilleras donde el
    levantamiento de las placas pasa un umbral). Una regla `Point` tiene exactamente una entrada:
    con varias entradas con peso, elegía en cada punto y hacía confeti de 256 bloques. El chequeo
    al congelar rechaza una regla `Point` con más de una.
- La elección entre entradas es por rendezvous hashing, con pesos enteros de 1 a 64. El puntaje de
  una entrada es el mayor `mix(seed, salt(regla), id de celda, salt(tipo), j)` con `j < peso`. Gana
  el puntaje más alto, y un empate va al id menor. Agregar una entrada de peso w cambia solo las
  celdas que ahora gana; quitarla devuelve el mapa viejo bit a bit.
- El selector es el único lugar donde se elige un tipo. Los chunks, la lejanía, las generaciones
  privadas de la caminata del cielo, la búsqueda del spawn y el F3 lo llaman, así coinciden.

### 5. Tipos de terreno y la mezcla

`TerrainKind<Cx>`:

- `id` y `version`;
- `form`: `Height` o `HeightAndVolume`;
- `limits`: la pendiente máxima, el rango de relieve y el rango de relieve dentro de una banda de
  mezcla;
- `bounds(cx, seed, q, reach)`: sus cotas de altura a menos de `reach` bloques del punto de
  retícula q (dos celdas de retícula), con la de abajo que da `ground_lo` (sección 14), y
  `range(cx)`, sus cotas en todo el mundo (el rango vertical del reino);
- `heights(grid, mask, out)` y `lod_heights(grid, level, mask, out, margin)`;
- en los tipos de volumen, `volume(...)` y `lod_runs(...)`.

Hay una llamada virtual por (columna de chunk, tipo presente) y por (nodo de la lejanía, tipo
presente). Los bucles de adentro son monomórficos.

El Context es del juego: un struct tipado por reino, monomórfico. Calcula los campos compartidos
una vez por columna de chunk o por nodo y expone parámetros con nombre como `i32`. Un tipo de un
mod escrito contra el Context del juego comparte todos sus campos caros, a cambio de depender del
crate del juego.

**Pesos.** Con `r = 1` y `N = (2r + 1)^2 = 9`:

```
n_k(q)  = cuántos de los 9 puntos de la caja de 3 x 3 alrededor de q tienen el tipo k
fx = x & (Ls - 1), fz = z & (Ls - 1)    (la columna p dentro de su celda, esquinas q00 a q11)
c_k(p)  = n_k(q00) (Ls - fx)(Ls - fz) + n_k(q10) fx (Ls - fz)
        + n_k(q01) (Ls - fx) fz       + n_k(q11) fx fz
suma_k c_k(p) = N Ls^2 = 9 * 2^16       (exacto, sin redondeo)
H(p)    = floor(suma_k c_k(p) H_k(p) / (9 * 2^16))
```

- La banda de transición mide `(2r + 1) Ls = 768` bloques, centrada en el borde.
- Desborde: `|suma_k c_k H_k| <= N Ls^2 * 2^31 < 2^51` con cualquier cantidad de tipos, así `i64`
  alcanza. No hay tope de 4 tipos por columna: una columna evalúa todo tipo con `c_k > 0`.
- La misma fórmula sirve para una columna, una grilla y una esquina de la lejanía, así un punto,
  un chunk y la lejanía coinciden bit a bit.
- Camino rápido del interior: si los 16 puntos de retícula alrededor de la celda de una columna de
  chunk tienen un solo tipo, una llamada a `heights` escribe H, sin multiplicar ni dividir.

### 6. La pendiente en los bordes

```
grad H = suma_k w_k grad H_k + suma_k (H_k - m) grad w_k      (para cualquier constante m)
```

- Un paso de la caja mueve los conteos de todos los tipos juntos a lo sumo 6 de 9 cada 256
  bloques. Con m el punto medio de las alturas presentes, el segundo término es a lo sumo `D / 768`
  por eje y `D / 543` (raíz de 2 veces más) en diagonal, con cualquier cantidad de tipos. D es la
  mayor `|H_a - H_b|` entre los tipos presentes en la columna (los de sus 16 puntos de retícula).
  El primer término es a lo sumo la mayor pendiente de esos tipos.
- Los tipos suman relieve sobre la base común del Context, así D es una diferencia de relieves.
  Cada tipo declara su rango de relieve `[lo_k, hi_k]` dentro de la banda, y para cada par
  `D_ab = max(hi_a - lo_b, hi_b - lo_a)`.
- El chequeo al congelar pide `max(slope) + max(D_ab) / 543 <= 3` para todo par y todo trío de
  tipos que las reglas puedan juntar en una misma ventana de 4 x 4 puntos de retícula, con los dos
  máximos tomados dentro del grupo. Con los tríos alcanza: un grupo más grande cumple si cumple el
  trío de su tipo más empinado y su par más lejano. Cada tipo solo también pide `slope <= 3`: es
  el interior de sus regiones.
- Solo por pares no alcanza: con `a` en [90, 110], `b` en [-110, -90] y un `c` más empinado en
  [-10, 10], cada par pasa con la pendiente máxima que le deja el chequeo y el punto triple llega a
  3,08.
- El tope de 3:1 vale en cualquier línea, no solo en los ejes. Es lo que piden los faldones de la
  lejanía (ADR 0031) y el test de pendiente.

### 7. Términos de núcleo y volúmenes

Para el mundo vertical, un tipo puede llevar relieve que existe solo en el núcleo de su región: las
agujas (suben), el agujero gigante y las montañas invertidas (bajan).

- `d_f(p)`: la distancia de Chebyshev entera de p al punto de retícula más cercano de otro tipo,
  con tope en el alcance del tipo, `D_max`. Es un mínimo de funciones 1-Lipschitz: continua, exacta
  y 1-Lipschitz ella también.
- `K = (r + 1) Ls = 512` es la distancia de resguardo: con `d_f >= K` todos los pesos de la mezcla
  están llenos, así un término de núcleo nunca toca la mezcla.
- Tope en cono, que crece desde la línea de resguardo (`s_c` es la pendiente del cono del tipo, 2,5
  por defecto):

  ```
  raise' = min(raise,  s_c * max(0, d_f - K))
  lower' = max(lower, -s_c * max(0, d_f - K))
  ```

  El término vale cero hasta K, y su pendiente es a lo sumo `max(pendiente propia, s_c)` en
  cualquier línea (d_f es una distancia de Chebyshev, 1-Lipschitz en toda dirección). La pendiente
  propia es la del término antes del cono.
- El término se suma a la altura mezclada del tipo, así el tope pide
  `slope_k + max(pendiente propia, s_c) <= 3` (chequeo al congelar), con `slope_k` la pendiente que
  el tipo declara en `limits`. Con `s_c = 2,5`, el tipo declara una pendiente de a lo sumo 0,5; si
  necesita más, baja `s_c`.
- La altura cuesta lugar, medido en `d_f`, que es de Chebyshev. Una aguja de 12 km necesita una
  `d_f` de unos 5,4 km en su pico, así su región tiene que contener un cuadrado de unos 11 km de
  lado. Su regla `Point` lo da si la distancia del `ContactField` también es de Chebyshev: un
  cuadrado de semilado de al menos 5,6 km alrededor del sitio de contacto. Un disco euclídeo de
  radio R da en su centro solo unos `R / raíz de 2` de `d_f`, porque el punto ajeno más cercano cae
  en la diagonal, así necesita un radio de al menos unos 7,9 km. V3 elige la distancia.
- Un agujero de profundidad P necesita una región que contenga un cuadrado de
  `2 (512 + P / 2,5)` bloques de lado.
- Costo: `d_f` se calcula por columna de chunk con los puntos ajenos de una ventana de radio
  `D_max / Ls`, leídos una vez del memo de la retícula (unos 45 x 45 puntos en una aguja, y solo en
  regiones de aguja). Si mide alto, la palanca es una cota conservadora por celda de región desde
  las aristas de Voronoi de WG1, usada igual por los chunks y la lejanía.

**Volúmenes** (islas flotantes, arcos):

- Un volumen existe solo donde `d_f >= K`. Su sesgo de densidad se escala por
  `ramp(d_f) = clamp((d_f - K) / (F Ls), 0, 1)` en punto fijo (F son las celdas de desvanecido, 2
  por defecto), así llega a cero justo en la línea de resguardo. Un sesgo de densidad no tiene tope
  de pendiente, así que acá una rampa sirve.
- Un chunk de volumen paga solo dentro de las bandas que declara su tipo. Fuera de ellas siguen
  los caminos `Uniform` y de 1 bit.
- El futuro de los volúmenes es la ranura de densidad 3D del diseño A (una estimación de la
  superficie más una banda): un solo mecanismo para islas y aleros.

Los términos de núcleo, `d_f` y los volúmenes se construyen en WG-VOLUME (V2), no en WG-REGION:
v1 no tiene quien los use.

### 8. Combinadores estáticos, sin DSL

- Structs genéricos `Lattice<C>` (evalúa C en los puntos de la retícula y después interpola
  bilineal exacto), `Spline`, `Add`, `Min` y `Blend`. Cada uno implementa `value`, `bound`, `fine`
  y `slope`. Se monomorfizan y no hay intérprete. Llegan con WG1.
- Tres arreglos al álgebra del diseño A:
  - `fine` se mide contra el interpolante bilineal de las esquinas de la celda, no contra el máximo
    de las esquinas;
  - `Min` lleva un término de cruce: `min(x, s - x)` vale 0 en las esquinas y tiene un pico de
    `s / 2` adentro;
  - el término de la mezcla sobre dos ejes es `(D / L) * s`, no `s / 2`.
- Los tipos pueden usarlos; nada los obliga.

### 9. La capa de decoración

Una capa compartida por reino:

```
context -> select -> alturas de los tipos -> mezcla -> MODIFICADORES DE FORMA -> regla de fluido
  -> bioma -> RELLENO -> SUPERFICIE -> CARVERS -> FEATURES -> ESTRUCTURAS -> POST
```

- Las fases son fijas. Dentro de una fase, el orden es `(priority, id)` (sección 11). Cada etapa
  tiene una sal explícita que sale de su id.
- **Modificadores de forma.** Corren sobre la grilla de alturas ya mezclada, con una forma por
  punto o grilla y una forma gruesa para la lejanía. Declaran su efecto: `LowerOnly` (ríos, lagos,
  cañones) o `Raise(max)` (escalones de acantilado, dunas; suma a la cota de la columna y al margen
  de la lejanía). `LowerOnly` no queda libre de margen: ver el término de divisoria (sección 10).
  Los ríos, los lagos y la forma de la costa son del reino entero, así cortan a todos los tipos
  igual.
- **Bioma.** Una tabla `BiomeRule`: el mecanismo es del motor y los datos del juego. Cada entrada
  es `{ bioma, tipos o etiquetas de tipo, intervalos sobre parámetros del Context, peso }`.
  - El tipo que entra es el dominante de la columna (el de mayor `c_k`, y un empate va al índice
    menor).
  - Gana la caja más cercana en el espacio de parámetros entre las entradas que coinciden, con
    rendezvous entre cajas iguales.
  - Un bioma restringido a su tipo no puede contradecir al terreno.
  - Las zonas 3D son a lo sumo 6 intervalos de profundidad por columna.
  - Los ids de bioma son etiquetas: nada se guarda por chunk.
- **Relleno** (la roca: estratos, provincias, basamento) y **reglas de superficie** por bioma, una
  llamada por (chunk, etapa, bioma presente), cada una con `lod_top` para la lejanía.
- **Carvers**: campos 3D puros, o carvers por celda de origen con alcance declarado. Quedan fuera
  de la lejanía.
- **Features**: anclas en celdas alineadas a chunks, con alcance y altura declarados. Decide el
  bioma de la columna del ancla. Forma gruesa opcional (una copa como tramos sólidos).
- **Pasos post**: reescrituras locales al chunk con consultas de solo lectura. Declaran que no
  mueven la superficie; si la mueven, necesitan forma gruesa.
- Las reglas de colocación (lo que queda bajo el agua va mojado, los estados generados son puntos
  fijos de sus reglas de vecinos y de apoyo, nada que cae queda sobre aire) son tests del juego
  sobre la salida de cada etapa, no reglas del motor.
- No se construye hasta que alguien lo pida: predicados de carga, modificadores de bioma con fases
  ADD, REMOVE y MODIFY, y reglas de superficie despachadas por espacio de nombres. Entran en los
  registros tal como son.

### 10. La lejanía (LOD) por regiones

Una sola fuente por dimensión, nunca una `LodSource` por tipo. Un nodo de nivel k mide `32 * 2^k`
bloques, así que desde el nivel 3 casi cualquier nodo puede cruzar un borde de tipos, y la fuente
nunca le pasa un nodo a un solo tipo. Por llamada, para las 35 x 35 esquinas del nodo acolchado:

1. La grilla del Context en las esquinas (juego, una vez).
2. Selección: los tipos de los puntos de retícula dentro del nodo más `r + F` celdas, el conjunto
   de tipos presentes, y `c_k` en cada esquina con la fórmula de la sección 5 (así una esquina sobre
   una columna de chunk da los bits del chunk). Desde el nivel 8 cada esquina es un punto de
   retícula y `c_k` es una lectura de conteo de caja.
3. Para cada tipo presente, `lod_heights` en las esquinas donde `c_k > 0` (máscara), con su margen
   por celda. La prueba de interior (un solo tipo) se hace por bloque de 4 x 4 esquinas, no por
   nodo.
4. La mezcla exacta de las alturas de las esquinas.
5. Tope grueso por celda. H es una combinación convexa de los `H_k`, así dentro de una celda
   `H <= suma_k c_k(p) T_k / (N Ls^2)`, con `T_k` el máximo de `H_k` en las esquinas más
   `margin_k`.
   - Hasta el nivel 8 la celda cae en una sola celda de retícula, la cota es bilineal y su máximo
     está en una esquina: `top = máximo en las 4 esquinas de suma_k c_k T_k / (N Ls^2)`. Es
     ajustada: una celda de llanura en el borde de una banda de montaña sube solo por su peso de
     montaña.
   - Desde el nivel 9, el máximo de `T_k` entre los tipos presentes.
   - Hoy `MAX_LEVELS` es 6. La pasada admite hasta el nivel 8 con selección exacta y documenta la
     selección solo en las esquinas más allá.
6. Las formas gruesas de los modificadores (ríos por nivel, costa, adaptación a estructuras),
   después la regla de fluido del reino sobre las esquinas sin margen, y después el `lod_top` del
   bioma.
7. Volúmenes y volúmenes de núcleo: `lod_runs` da hasta 4 tramos sólidos por columna de celdas,
   que se unen a los de la superficie.
8. Una pasada genérica de la regla de la celda (del motor: la `Surface` del demo, generalizada)
   emite `LodNode::Uniform` o `Cells` y la banda, con `below` = el `floor` del reino dibujado.

**Márgenes.**

- Las octavas que se dejan de lado, multiplicadas por la pendiente local del spline.
- Por cada retícula más fina que la celda, su cota de pendiente local por `s / 2`, medida contra el
  interpolante bilineal.
- El término de cresta: donde las esquinas no coinciden en el signo del campo de crestas, la
  cumbre queda dentro de la celda.
- Conos y dunas, su propia cota.
- El término de mezcla, `(D / Ls) * s`, dentro de una banda, para celdas más chicas que Ls.
- Los términos de núcleo, `max(pendiente propia, s_c) * s` (el cono es un mínimo, así que lleva el
  término de cruce).

**Término de divisoria.** Un corte `LowerOnly` como `H = min(H, floor + (H - floor) g(n))`, con n
sacado de un mínimo sobre segmentos, puede dejar una divisoria dentro de una celda cuyas 4 esquinas
están cortadas. El tope grueso quedaría entonces bajo el real, hasta la pendiente de la pared por s.

- Regla, la análoga para ríos del término de cresta: donde el segmento más cercano (o el orden más
  cercano) cambia entre las esquinas de una celda, el tope de la celda usa el máximo en las
  esquinas de la H sin cortar, más su margen, como si ahí no hubiera corte. Es conservadora y solo
  toca las celdas que cruzan una divisoria.
- Donde las 4 esquinas comparten su segmento más cercano A, la distancia a A es convexa, así sin
  deformación n dentro de la celda no pasa el máximo de sus esquinas. La deformación de los
  meandros (Lipschitz 1/2) suma a lo sumo unos 1,1 s, así el corte de la celda usa
  `n_max = máximo de n en las esquinas + 1,1 s` (g crece con n: un n más grande corta menos).

Además:

- Las costas no se mueven con el nivel: la regla de fluido lee las esquinas sin margen, y el suelo
  de una celda mojada tiene tope en `datum - 1`.
- Los valles que se dejan de lado en los niveles lejanos solo suben el suelo grueso. No piden
  margen; el error de arriba que admite el test crece en su profundidad.
- Memo: un struct en `LodScratch.memo`, con una ranura por `KindIx` y una para el Context, y la
  llave del contador de construcción (sección 13).
- Las ediciones se superponen a las celdas como hoy (ADR 0031 sección 11). El contrato de la fuente
  no cambia.

**Reinos en la lejanía.**

- Un solo reino dibujado por dimensión (`lod = Draw`). La fuente de la dimensión dibuja solo la
  banda de ese reino, con `below` = su `floor`. Los otros reinos nunca están en la lejanía, se los
  mire desde donde se los mire.
- Reinos de abajo (el Nether): la banda de la superficie deja `lo` cerca de su superficie más honda
  con `below` = su `floor`, así el Nether no se dibuja, como no se dibujan las cuevas.
- Reinos de arriba (el techo): por contrato, las capas desde `hi` son aire, así el techo no se
  dibuja con ningún preset (sin eso, el horizonte 3D de 49 km de `extreme` lo alcanzaría). Una
  aguja se dibuja hasta el plano y se pierde ahí en la bruma.
- Un reino que sube para siempre deja `vertical_chunks().end()` sin límite, y el lado del mundo de
  la lejanía querría capas vacías hasta el horizonte 3D. WG4 le da a la lejanía un techo por
  columna de nodos desde las cotas del reino dibujado (sección 14), así el conjunto querido queda
  tan ajustado como hoy. V4 lo necesita; V1 no (el Nether está abajo, y el rango de la superficie ya
  empieza en `i64::MIN`).
- Cámara en un reino con `lod = Off` (el Nether, la roca y las cuevas del techo): el juego pone
  `LodSettings.enabled = false` a través del entorno (sección 16). No corre ningún job, no se dibuja
  nada, y lo lejano es la niebla y el color de cielo del entorno. Al volver, la lejanía se llena de
  cero, como después de un salto. Sin cambios en el motor. En el aire bajo la cara de abajo del
  techo, el entorno deja el LOD encendido, así la superficie sigue dibujada abajo.
- Las caras de la lejanía se iluminan como cielo abierto (ADR 0031 secciones 9 y 12). Está bien
  para el reino dibujado, que es `Open`. Dibujar lejos un reino `Closed` o `Under` (islas del cielo
  vistas desde la superficie, la cara de abajo vista desde abajo) es EW-REALMLOD, y no está
  planeado: pide un árbol por reino y una paleta de luz por nodo.
- Una aguja de 12 km son 24 capas de nodos en el nivel 4 y 6 en el nivel 6: varios jobs por columna
  en los niveles cercanos, en columnas raras, que cuenta V3. Una torre fina queda al menos como una
  columna de celdas en todo nivel (cualquier bloque sólido hace sólida su celda).

**Harness exportado.** `boxel_worldgen` gana la feature `test-support` con
`check_lod(generator, columns, levels)`, que arma chunks reales y compara en los niveles 1 a 4:

- en celdas secas, el tope grueso nunca queda bajo el real, y lo pasa a lo sumo por el margen
  declarado más la profundidad de los valles que se dejaron de lado;
- los desacuerdos de fluido, solo a menos de s de un borde de fluido;
- bandas conservadoras, bordes iguales entre vecinos y tramos de volumen dentro de su cota.

El juego y cualquier tipo de un mod lo llaman con sus propias columnas, así un `lod_heights` escrito
a mano queda probado sin tests de lejanía a medida.

### 11. Ids, registros, sales y versiones

- `GenId`: un nombre con espacio de nombres y la misma regla que los bloques (`validate_name` de
  `boxel_voxel::registry`). Se registran dimensiones, presets, reinos, tipos de generador,
  reglas del selector y sus entradas agregadas, tipos de terreno, envoltorios de tipo, modificadores
  de forma, biomas, etapas de decoración, entornos y etiquetas; más adelante, sets de estructuras
  (#116).
- `GenRegistry<T>`: lo arman los plugins al construir la app y se congela antes de abrir cualquier
  mundo (ADR 0034 sección 1). Al congelar se ordena por `GenId` (o por `(fase, priority, id)` en las
  entradas con orden), nunca por orden de registro ni de hash, y se dan índices densos en ese
  orden.
- Registrar nunca falla. Los duplicados se rechazan al congelar, todos juntos en un solo error.
- La prioridad va de mayor a menor, igual en las reglas del selector y en las etapas de la
  decoración: la más alta decide o corre primero, y la regla de reserva, de prioridad mínima,
  decide última.
- `salt(id) = splitmix64(fnv1a64(bytes del id))`, mezclada en la semilla del reino, nunca sumada:
  un fbm le da a la octava k la semilla `seed + k`, así una sal sumada podría caer sobre otra
  octava. Un test comprueba que no chocan dos sales ni dos semillas de octava.
- Versiones: toda entrada tiene `version: u32`. El hash de parámetros incluye el orden de la
  entrada (fase y prioridad), así un cambio de prioridad es una entrada cambiada.
- `RegistryDigest` es la lista ordenada de `(id, version, hash de parámetros)` de cada entrada a la
  que llega un preset, por dimensión y por reino, más su hash de 64 bits. Una línea se identifica
  por `(alcance, registro, id)`, con el alcance = dimensión y reino: un id es único solo dentro de
  su registro (un reino y su entorno pueden llamarse igual).
- `WorldGenerator::version()` devuelve los 32 bits bajos del digest de su propia dimensión
  (`RegistryDigest::dimension`), así un cambio en otra dimensión no la sube.
- Lo que no es una entrada de registro tiene igual su línea en el digest del reino: `context` (la
  versión y los parámetros del Context, que declara sus propios parámetros con nombre), `region`
  (la versión del código del motor y el paso de la retícula) y las etapas de la decoración.

### 12. Chequeos al congelar y al abrir

Corren en tiempo de ejecución, no solo en CI: al construir la app para los registros, y al abrir un
mundo para el conjunto que resuelve su preset. Rechazan:

- ids desconocidos y duplicados (también un parámetro que el Context no declara y una entrada
  agregada a una regla que no existe);
- una regla `Point` con más de una entrada, o una regla `Cell` sin ninguna;
- un peso fuera de 1 a 64, un tipo dos veces en una regla, y celdas de región más chicas que la
  retícula;
- un tipo cuya forma todavía no está hecha, o cuyo relieve en la banda sale de su relieve;
- envoltorios que apuntan a ranuras que no existen;
- un tipo con una pendiente de más de 3, o un par o un trío de tipos que pueden juntarse y no
  pasan el chequeo de pendiente y relieve de borde (sección 6). Las condiciones no dicen dónde se
  cumplen, así el chequeo toma todo par y todo trío de tipos del reino;
- términos de núcleo cuyo cono o pendiente propia, sumados a la pendiente de su tipo, rompen el
  tope (sección 7);
- features sin alcance o sin altura declarados;
- spans de reinos que se pisan o que no están en planos de 4096 bloques, o más de 8 planos por
  dimensión (el tope que hace O(1) a `realm_of`);
- reinos que dejan algún y sin cubrir, así `realm_of` siempre responde;
- planos fuera del mundo (más allá de +-`MAX_BLOCK_COORD`); un extremo abierto no es un plano;
- un mapa de regiones sin regla de reserva (sección 4);
- una forma que no coincide con la política del `floor`, o que todavía no está hecha;
- una dimensión sin exactamente un reino `Draw`.

Una mezcla de mods rechazada falla al cargar, con un mensaje que nombra los ids, nunca en pleno
vuelo.

### 13. Memos, el chequeo de spans y el lado de consultas

- **Llaves de memo.** Un contador `AtomicU64` del proceso se toma al construir un
  `RegionGenerator` (o la pila). Todo memo por worker (sitios, puntos de retícula, tipos, aristas de
  placas, esquinas de la lejanía) lleva esa llave y se vacía si no coincide. El juego arma un
  generador nuevo en cada apertura, así abrir el mundo A, cerrarlo y abrir el B en los mismos
  workers nunca lee el memo de A.
- **Chequeo de spans en debug.** En los builds de debug, `generate` comprueba que todo bloque que
  no es aire queda dentro de las cotas de columna del reino y bajo la cota local de aire, y que los
  bloques de volumen están solo donde `d_f >= K`. Los tests del motor corren en debug, así un tipo o
  un mod con una cota mal hecha falla sus tests.
- **Lado de consultas.** `WorldQuery { height_at(x, z), realm_at(y), biome_at(x, y, z),
  column(x, z) -> ColumnInfo, environment_at(x, y, z) }`: puro, unos 2 us en caliente, por
  `as_query()`. Hasta que exista la tabla `BiomeRule`, `biome_at` da una etiqueta provisoria: el
  tipo dominante de la columna, que es la entrada de esa tabla. La búsqueda del spawn, la línea
  del F3, la niebla y los cambios de entorno lo usan en vez de tipos concretos del demo.

### 14. Cotas verticales por columna

Dentro del generador, sin cambiar el motor (desde W1):

- Por (reino, columna de chunk): la unión de las cotas de los tipos presentes en sus 16 puntos de
  retícula, más los deltas declarados de los modificadores (los ríos solo bajan), las features (la
  altura de los árboles), los carvers (su profundidad) y las estructuras (las cajas de sus piezas).
  Son enteros exactos y 16 lecturas del memo.
- Las mismas lecturas dan `ground_lo`, una cota de abajo de la superficie abierta: la cota de
  altura más baja de los tipos menos el corte más hondo (carver, río o término de núcleo). Solo la
  lee el piso del cielo (sección 15).
- Un chunk sobre la cota local de arriba es aire `Uniform` tras las lecturas de selección (unos
  2 us). Un chunk bajo la cota local de abajo toma el camino hondo, sin la etapa de columna.
- Esto hace barato generar el aire de la banda, no transmitirlo: cada chunk de la banda sigue
  siendo una slot y un job de generación.

**WG4, la dirección** (#98; protegido: `known_air` de `streaming.rs` y el techo del lado del mundo
de la lejanía; un OK del dueño):

```
fn column_bounds(&self, dim, realm: RealmIx, area: ColumnArea) -> ColumnBounds
ColumnBounds { layers: RangeInclusive<i64>, ground_lo: i64 }
```

- `ColumnArea` es un cuadrado alineado de 2^n x 2^n columnas de chunk: n = 0 para una columna de
  chunk, n = k para una columna de nodos de nivel k (un nodo mide 2^k chunks de lado).
- `layers` son las capas de chunk que pueden tener algo que no es aire. `ground_lo` es un vóxel en
  o bajo la superficie abierta más baja del reino en el área; en un reino `Ceiling`, al revés: en o
  sobre el punto más alto de su cara de abajo.
- Por defecto, `vertical_chunks()` recortado al span del reino, con `ground_lo` = su vóxel de
  arriba. Así todo generador sigue como hoy, la luz incluida.
- `RegionGenerator` responde desde los puntos de retícula que cubren el área: unos 1 us en
  caliente y menos de 2 us en frío por columna de chunk. Una columna de nodos de nivel 6 lee unos
  100 puntos de retícula, nunca sus 4096 columnas de chunk.
- Lectores: `known_air(y)` pasa a `known_air(pos)`, memorizado por columna en el hilo principal, y
  el techo de la lejanía por (nivel, columna de nodos), desde las cotas del reino dibujado y subido
  por las ediciones como hoy. La caminata del cielo es lectora de EW-REALMSKY, no de WG4.
- Lo necesitan el reino del techo, las agujas y Amplified. No lo necesitan W1, W2 ni el Nether (que
  es sólido alrededor de su caverna, en la banda como cualquier chunk hondo).

La cadena entera: span del reino, cotas de columna del reino (WG4), cota local del chunk y chequeo
de spans en debug.

### 15. Luz del cielo por reino (dirección)

Esta sección es la dirección acordada, no el código. La luz es zona protegida: EW-REALMSKY (V3) y
EW-SKYUNDER (V4) piden cada uno el OK del dueño cuando les toca.

- **Open.** Un vóxel del reino r tiene cielo directo si `y > T_r(x, z)`. `T_r` camina solo el
  contenido de r y no lee ninguna capa fuera de su span.
  - Comienzo: `S_r = min(vóxel de arriba del span de r, max(vóxel de arriba de
    column_bounds(r, columna).layers, bloqueador editado más alto en r))`.
  - Piso: `F_r = max(vóxel de abajo del span de r - 1, ground_lo(r, columna) - límite)`. Una
    columna todavía abierta en el fondo de su span recibe `T = fondo del span - 1`: el cielo directo
    se corta en el plano y el reino de abajo sigue su propia regla.
  - Las ediciones nunca mueven un piso (la intención del contrato de hoy, ADR 0015 sección 4). El
    piso es una función pura de la columna del generador, ya no del tope global del mundo.
- **Por qué `ground_lo` y no `S - límite`.** Las cotas de columna son flojas en pendiente: el tope
  de una columna de chunk sale de 16 puntos de retícula sobre 768 bloques, así en una pendiente de
  2,5 puede quedar unos 1900 bloques sobre la superficie real. Un piso en `S - límite` quedaría
  sobre la ladera de una aguja o el fondo de un agujero, y los dejaría oscuros. `ground_lo` está
  bajo la superficie abierta por construcción: toda superficie abierta queda iluminada, y los pozos
  naturales reciben cielo hasta el límite bajo el suelo más bajo de la columna (la regla de hoy,
  medida por columna).
- **Under** (EW-SKYUNDER). Un vóxel de un reino `Under` tiene cielo directo si `y < B_r(x, z)`, con
  `B_r` el primer bloqueador que se encuentra caminando hacia arriba desde el plano de abajo del
  reino, con tope en el `ground_lo` espejado de la columna más el límite (el espejo del piso).
  - En el reino del techo ilumina el aire entre el plano y la cara de abajo: la cara de abajo y las
    montañas invertidas se ven iluminadas por el cielo, con día y noche.
  - Las capas bajo la cota de la cara de abajo son aire conocido (WG4) y la caminata las salta.
  - Un bloque que un jugador pone en ese aire le da sombra a la columna de arriba (la luz viene de
    abajo).
- **Closed.** Sin cielo directo. La luz que se esparce desde un vecino iluminado cruza el plano
  igual: las cajas de luz cruzan el plano como hoy, y una caja a menos de 16 vóxeles de un plano lee
  las reglas de los dos reinos.
- En el juego: la superficie es `Open`, el Nether `Closed`, el techo `Under` (su roca y sus cuevas
  quedan sobre el primer bloqueador y siguen oscuras como cualquier cueva) y unas islas del cielo
  futuras, `Open`.

Por qué por columna y por reino, con números. Hoy el piso es el tope global menos 1024
(`sky_walk_limit`):

- Con las montañas de W2 (tope cerca de y 500), el piso queda cerca de y -524: bien.
- Amplified (tope cerca de SEA + 1000) lo deja cerca de y 40: los pozos secos bajo él pierden el
  cielo directo. Tolerable.
- Una aguja de 12 km lo deja cerca de 11 264: toda columna de llanura queda sin cielo directo.
- Un reino de techo que sube para siempre deja el rango sin límite: la regla pasa a `SkyRule::Open`
  y toda cueva queda iluminada.
- Un agujero más hondo que el piso queda oscuro en el fondo.

Empezar la caminata por columna solo ahorra trabajo y no arregla ninguno de estos casos. Los pisos
por columna desde `ground_lo` sí.

Consecuencias del modelo:

- Ningún reino le da sombra a otro. El techo y las montañas invertidas no proyectan sombra de cielo
  sobre la superficie, y el sol de la superficie también ilumina la cara de abajo. Es una decisión
  de aspecto, barata y estable.
- Bajo una isla flotante, el fondo del agujero tiene sombra como bajo cualquier alero.
- La luz que entra por una brecha al Nether se corta en el plano: abajo solo queda la luz que se
  esparce (15 bloques) y la lava del propio Nether.
- Una torre que un jugador construye hasta el plano sube solo su propia columna, como las ediciones
  de hoy. Sus bloques sobre el plano son del techo y dan sombra solo hacia arriba.
- En un mundo de un solo reino, S pasa a ser el tope de la columna en vez del tope global
  (caminatas más cortas, menos generaciones privadas), y el piso sigue el suelo más bajo de cada
  columna. Solo los pozos naturales hondos bajo columnas bajas ven una diferencia: más cielo
  directo. Los techos de cielo no se guardan, así el guardado no cambia.
- Costo: EW-REALMSKY es M (el comienzo y el piso de la caminata en `boxel_light`, y en
  `boxel_world` la `SkyRule` por reino y el atajo de "caja bajo el piso", que usa el piso más bajo
  de sus 3 x 3 tiles). Una caminata abarca a lo sumo `(S - ground_lo) + límite`: el límite en
  columnas planas, como hoy, y más solo en columnas con pendiente o agujeros, donde igual se corta
  en el primer bloqueador. EW-SKYUNDER es de S a M (la caminata espejada y su regla de cajas).

### 16. Entorno y gravedad por reino

- Una tabla de entornos (datos del juego, un `GenId` por reino y más adelante por bioma): la curva
  del día y el piso de ambiente (`DaylightCurve`), las capas de niebla y las nubes (`FogSettings`),
  la niebla de distancia (`DistanceFog`), el color del cielo, el LOD encendido o apagado
  (`LodSettings.enabled`), y más adelante el signo de la gravedad.
- El juego los cambia según `environment_at(cámara)` por el lado de consultas, con una mezcla de 64
  bloques al cruzar un plano para que el cambio no sea un corte (el LOD usa la misma histéresis de
  64 bloques). `environment_at` es por posición, no solo por reino: el aire bajo la cara de abajo
  del techo usa un entorno como el de la superficie (LOD encendido, sol), y su roca y sus cuevas uno
  oscuro (LOD apagado).
- Los reinos `Closed`, y las partes oscuras de los demás (la roca y las cuevas del techo, que es
  `Under`), usan una curva sin sol y su propio ambiente (en el juego, el Nether cerca de 0,15 y las
  cuevas del techo 0,02, como las de hoy). Su color de cielo es el de su niebla, así no se ve nada
  más allá del radio cargado.
- El día y la noche siguen siendo globales: un reloj por mundo. Los reinos `Closed` no lo ven.
- EW-REALMENV (render, protegido) existe solo si alguno de esos recursos resulta leído una sola vez
  al arrancar. `FogSettings` cambia en tiempo de ejecución y `DaylightCurve` se lee en cada frame.
  Falta mirar `DistanceFog` (un campo de `RenderSettings`) en V0.
- Gravedad: hoy es un solo vector para todo el mundo (`PhysicsSettings.gravity`). La gravedad
  invertida por región es EW-GRAVITY: un campo de gravedad (función pura de la posición, el reino y
  la región, solo el signo), cuerpos que lo muestrean en cada paso, el "arriba" del controlador, el
  giro de la cámara, bloques que caen y fluidos que corren según el abajo del reino, y reglas de
  colocación colgante. El determinismo se mantiene (cambios de signo, sin libm). Es interior de la
  física, protegido y de tamaño L, y va último, después de la gran ronda de control. Sería un plugin
  opcional; sin él la gravedad es la constante de hoy y nadie paga.

### 17. El guardado

- Una clave de metadatos nueva del motor, `boxel:worldgen`. Se escribe solo al crear el mundo,
  junto a `boxel:generator`, y entra en `RESERVED` de `tools/no_content.py`. El store escribe
  `boxel:generator` en cualquier mundo que no la tenga, pero `boxel:worldgen` no: un mundo viejo no
  la recibe al abrirse, así sigue abriendo como legado.
- `FORMAT_VERSION` no cambia. El store rechaza cualquier versión que no sea la actual y no tiene
  lector de versiones viejas, así subirla rechazaría todas las partidas guardadas. El valor lleva su
  propio byte de versión del registro, así el registro puede crecer sin tocar `FORMAT_VERSION`.
- Contenido:
  - el id del preset;
  - la tabla de nombres de dimensiones (`DimensionId` u16 -> `GenId`, que solo crece, como
    `IdMap`);
  - por dimensión, la tabla de reinos (id del reino y span), el spec del generador (id del tipo,
    versión y hash de parámetros) y la lista del digest.
- Al abrir, el motor compara lo guardado en `boxel:worldgen` con los registros vivos y le devuelve
  al juego `WorldgenDiff { added, removed, changed }`. La política es del juego. La que eligió el
  dueño para el juego: avisar y seguir. Los chunks que no se guardaron salen del conjunto nuevo,
  así la tierra guardada puede encontrarse con tierra nueva en una costura.
- Agregar un reino a un mundo existente es una entrada agregada, más un span cambiado en el reino
  que corta (en el reparto del juego, los últimos 256 bloques de la superficie sobre -4096 pasan a
  graduar hacia la roca del Nether). Solo muestran costura los chunks ya guardados ahí abajo: en la
  práctica, ninguno.
- Los mundos viejos no tienen `boxel:worldgen`. Abren como legado: el juego reconstruye su
  generador desde el tipo de mundo que guardó en sus propios metadatos, con una fábrica de legado,
  sin cambios. Ningún mundo guardado gana costuras por este ADR.

### 18. La puerta de las estructuras

El diseño de las estructuras es de #116. Acá queda solo la puerta.

- `StructurePlanner` por reino: `plan(cell) -> pieces`, una función pura de
  `(seed, salt(set de estructuras), id de celda)` sobre celdas grandes (2^14 bloques por defecto,
  con su propia retícula). Lee el Context y el selector en pocos puntos, y los planes de las 3 x 3
  celdas vecinas, así las ubicaciones nunca se pisan y cualquier chunk puede listar toda pieza cuya
  caja lo toca.
- Las piezas tienen anclas y cajas declaradas. Nada escribe fuera de su chunk, así ninguna
  estructura queda cortada (la queja del dueño con Minecraft).
- Adaptar el terreno es un modificador de forma que lee las piezas cerca de una columna (aplanar,
  enterrar, pedestal), con su forma gruesa, así la lejanía muestra el suelo adaptado. Lejos, una
  pieza puede dar `lod_runs`: una torre queda como una columna de celdas sólidas.
- Los planes son puros. Calcular todas las estructuras cerca del spawn al crear el mundo es
  calentar una caché detrás de la pantalla de carga, nunca estado guardado. Una estructura saqueada
  o rota son ediciones comunes.
- La fase `STRUCTURES` va entre `FEATURES` y `POST`. Anclas, alcance y sales siguen las reglas de
  las features.

### 19. Qué pueden y qué no pueden hacer los mods

#115 decide cómo se cargan los mods. Este ADR solo deja las puertas abiertas.

- Supuesto: un mod es un plugin que registra entradas antes del congelado, en el mismo momento que
  el registro de bloques (ADR 0034), en Rust primero. Cada entrada es un id más un valor tipado
  (datos planos y un trait object opcional), así archivos de datos o un sandbox pueden producir los
  mismos valores después sin cambiar los registros.
- **Agregar un bioma con su terreno**: registrar un tipo (contra el Context del juego, o de
  volumen), una entrada en una regla abierta del selector (las reglas aceptan entradas por id, por
  ejemplo `game:land_cells`) con peso y condición, y una entrada de `BiomeRule` restringida a ese
  tipo. El rendezvous le da cerca de `w / (W + w)` de las celdas elegibles (W es la suma de los
  pesos que ya estaban); las demás conservan su tipo. Su pendiente y su rango de relieve en la
  banda pasan el chequeo con cada par y cada trío de tipos que puede tener cerca (sección 6), o el
  mundo no abre y el mensaje nombra los ids.
- **Modificar lo de serie**: `KindWrapper` y `ContextWrapper` por `(priority, id)`, y
  modificadores de forma, reglas de superficie, carvers, features, pasos post y sets de estructuras
  agregados por id a su fase. Una entrada `Disable(id)` de prioridad mayor apaga una.
- **Agregar un reino**: un `RealmSpec` con su span en planos de 4096. Puede cortar el span abierto
  del reino de arriba o del de abajo en un plano libre (en el juego, bajo -8192 por defecto), y eso
  es una entrada cambiada de ese reino en el digest. Cualquier otro solapamiento se rechaza al
  congelar. Un reino trae su Context, su selector, sus tipos, su decoración y su entorno, así dos
  mods de reinos nunca se tocan.
- **Reemplazar el generador**: registrar un tipo de generador y un preset. La API dice que eso no
  se combina con otros mods de terreno.
- Lo que el motor mantiene: registros que solo se agregan, el orden `(fase, priority, id)`, sales
  desde los ids (agregar una entrada nunca mueve el azar de otra), la elección por rendezvous, los
  chequeos al congelar en tiempo de ejecución, el chequeo de spans en debug, el harness de la
  lejanía para los tipos de un mod, y el digest guardado con cada entrada y su versión. Un mod
  quitado o agregado es un `WorldgenDiff`, nunca un cambio silencioso.
- **Lo que un mod nunca puede hacer**: escribir fuera del chunk que genera, leer bloques de otro
  chunk, depender del orden de generación, sembrar por posición en una lista, o cambiar la
  superficie sin una forma gruesa.

### 20. Alcance de v1 y orden de los pasos

- v1 (W0 y W1) es solo el reino de la superficie, con un tipo de tierra (`game:continental`). No
  corre mezcla en el camino caliente hasta que W2 la mida: el camino rápido del interior corre en
  todas partes, y el selector tiene una regla `Cell` con una entrada.
- La API de reinos y regiones existe desde WG-REGION (el paso 4 de abajo, el primero que genera
  chunks), con un reino, la forma `Ground` y tipos de test, así nada se reescribe cuando llegue el
  mundo vertical. El streaming, la luz y el lado del mundo de la lejanía no cambian en v1. Ningún
  término de núcleo, volumen ni otra forma se construye antes de su paso V.
- Pasos del motor antes del primer vuelo, en orden:
  1. WG-ADR: este ADR, en paralelo con WG1 y W0, antes de WG-REG.
  2. WG1 #67: primitivas exactas (ruido de gradiente 2D, deformación entera, ruido celular con ids
     de sitio, distancia exacta a segmento, spline en punto fijo) y los combinadores de la
     sección 8.
  3. WG-REG: `GenId`, `GenRegistry`, orden del congelado, sales, `RegistryDigest`, `WorldgenDiff`
     (todavía sin store), el contador de construcción y el marco de los chequeos.
  4. WG-REGION: el selector, la mezcla, `RegionGenerator` con tipos de test, `FluidRule`, la pila de
     reinos con la forma `Ground` (un reino `Draw`), las cotas locales, el chequeo de spans, los
     métodos con defecto y el lado de consultas. De la decoración, solo las etapas de relleno y
     de superficie; el resto de las fases llega con quien las pida.
  5. WG-REGIONLOD: la pasada de celdas, las máscaras, el tope bilineal, el término de divisoria con
     el de la deformación, solo tramos de `Ground` y el harness.
  6. WG-SAVE: `boxel:worldgen` sin cambiar el formato y en `RESERVED`, presets, tablas de
     dimensiones y reinos, y la fábrica de legado.

  El mapa del juego (W0) corre justo después de WG1, antes de cualquier PR del marco, así el dueño
  juzga la escala y el clima sobre imágenes temprano.
- El mundo vertical, idea por idea:

| paso | idea | mecanismo | items del motor |
|---|---|---|---|
| V0 | entorno por reino | tabla de entornos y cambio según la cámara; tipo de mundo "Vertical" | EW-REALMENV, solo si hace falta |
| V1 | el Nether abajo | reino `Cavern`, `Closed`, `lod Off`, mar de lava por `FluidRule` | WG-CAVERN |
| V2 | agujero gigante con islas | tipo `HeightAndVolume`: núcleo que hunde, islas como volumen, tierra adentro y seco | WG-VOLUME |
| V3 | agujas de 12 km | tipo con núcleo que sube, regla `Point` sobre la distancia del `ContactField` | WG4, EW-REALMSKY |
| V4 | techo del cielo | reino `Ceiling`, `Under`, `lod Off`; ciudades como estructuras | forma `Ceiling`, EW-SKYUNDER (más WG4 y EW-REALMSKY de V3) |
| V5 | bioma al revés | tipo del techo con montañas invertidas en los sitios de contacto de V3 | ninguno más |
| V6 | torre con escalera | set de estructuras (#116) que puede romper el techo por una huella de contacto | la puerta de estructuras |
| V7 | gravedad invertida | campo de gravedad | EW-GRAVITY |

- Los items del motor, con su zona y su tamaño:

| item | zona | tamaño | paso |
|---|---|---|---|
| WG-CAVERN (forma `Cavern`) | `boxel_worldgen`, no protegido | S | V1 |
| WG-VOLUME (`d_f`, cono, volúmenes, `lod_runs`, fluido antes de los núcleos) | `boxel_worldgen`, no protegido | M | V2 |
| WG4 #98 (`column_bounds`) | streaming y lado del mundo de la lejanía, OK | M | V3 |
| EW-REALMSKY | luz, OK | M | V3 |
| forma `Ceiling` | `boxel_worldgen`, no protegido | S | V4 |
| EW-SKYUNDER | luz, OK | S a M | V4 |
| EW-REALMENV | render, OK | S | solo si un recurso no se puede cambiar |
| EW-GRAVITY | interior de la física, OK | L | V7 |

- Orden del dueño: V0 y V1 después de W2, V2 después de W3, V3 a V6 después de W4, y V7 después de
  la gran ronda de control. El Nether entra al mundo de serie del juego cuando se acepta V1; el
  agujero, las agujas y el techo quedan en el tipo de mundo "Vertical" hasta que la ronda de control
  los mida.
- Un OK del dueño por item protegido, cuando le toca. Del mundo vertical: WG4, EW-REALMSKY,
  EW-SKYUNDER, EW-REALMENV solo si hace falta, y EW-GRAVITY después de la gran ronda de control.
  Del resto de la generación, fuera de este contrato: EW-SNOWLOD (después de W2), EW-TINT (W2), WG6
  (W3) y WG3 solo si una medición lo pide.
- No planeado: dos dimensiones cargadas a la vez; un Nether como dimensión aparte con portales (el
  reino lo reemplaza); dibujar un segundo reino en la lejanía (EW-REALMLOD).

## Presupuestos y medición

La generación corre en workers: su costo se ve como tiempo de llenado después de cruzar un borde,
no como tiempo de frame.

- Despacho por reino: a lo sumo 50 ns por chunk.
- Selección por columna de chunk: a lo sumo 0,5 us en caliente y 2 us en frío amortizado.
- Chunks de WG-REGION (de superficie, hondos y de aire) contra un gemelo de un solo tipo hecho a
  mano: en columnas interiores, a lo sumo un 5 % más; en columnas de borde, a lo sumo 12 us más.
  Un chunk de aire sobre la cota local, a lo sumo 2 us de generación.
- Lejanía: la fuente a lo sumo 0,15 ms y el job entero a lo sumo 0,4 ms (la meta del ADR 0031 es
  0,3) en nodos interiores, de 2 tipos y de 3 tipos, en los niveles 1 a 4. Los niveles 5 y 6 (solo
  los usa `extreme`) se registran en la PR contra los mismos números. Un nodo paga unas
  1225 x (1 + parte de borde) evaluaciones de tipo (unas 1,4 veces en el nivel 6) más 1 a 3 us de
  mezcla; lo que más pesa sigue siendo la grilla del Context.
- WG4: a lo sumo 2 us en frío y 1 us en caliente por columna de chunk; unas 100 lecturas de retícula
  por columna de nodos.
- Consultas: `biome_at` en la columna de la cámara, a lo sumo 20 us en frío y 2 us en caliente, una
  vez por cambio de columna.
- Memoria: el memo por worker, a lo sumo 256 KiB con el de selección adentro; el de la lejanía, a lo
  sumo 512 KiB por worker de lejanía; sin caché de regiones.
- Medición: solo el nivel 1 en cada paso (AGENTS.md). WG-REG y WG-SAVE son solo tests (WG-SAVE
  corre una vez `just perf-smoke` porque toca `persist`). WG-REGION mide con criterion la selección
  y los chunks contra el gemelo de un solo tipo, y `headless_world` da los mismos quads y chunks en
  el demo. WG-REGIONLOD mide la fuente y el job en los niveles 1, 4 y 6. No hay ronda de control
  hasta terminar la generación y el trabajo de bloques del motor.

## Cómo se prueba

Los tests del motor usan ids `test:`; los del juego, su contenido.

1. **Exactitud**: punto contra grilla de chunk contra esquina de la lejanía, bit a bit, en el
   origen, en +-2^31, en 2^40 y cerca de +-`MAX_BLOCK_COORD` (ADR 0014), para cada primitiva y
   combinador, el tipo y los `c_k` del selector, la mezcla, `d_f` y el cono. La suma de los `c_k` es
   exactamente `N Ls^2` en todas partes (proptest).
2. **Desborde**: la mezcla con muchos tipos presentes (tipos de test en alturas extremas, más de 4
   por columna) nunca desborda `i64` y coincide con una referencia en `i128`.
3. **Determinismo**: un bloque de 6 x 6 x 6 chunks generado en orden mezclado en N hilos da las
   mismas huellas FNV con cada memo frío, caliente, desalojado y apagado. Abrir el mundo A, cerrar y
   abrir el B en los mismos workers da las huellas de B. No chocan sales ni semillas de octava.
4. **Registros**: registrar en órdenes mezclados da los mismos índices, sales y digest. Los
   duplicados y una regla `Point` con dos entradas se rechazan. Cada chequeo al congelar tiene un
   test de que una combinación mala falla al abrir con los ids en el mensaje.
5. **Aditividad**: agregar una entrada de test de peso w cambia solo las celdas que gana (proptest
   sobre semillas), y quitarla devuelve el mapa viejo bit a bit. Agregar un reino o una etapa no
   cambia la salida de los demás.
6. **Costuras y pendiente**: las columnas de borde son iguales desde cualquiera de los dos chunks.
   `|dH| <= 3` en líneas al azar (diagonales incluidas) a través de bordes de tipos, bordes de celda
   de retícula, puntos triples y bordes de términos de núcleo. Cortar nunca sube una columna.
7. **Volúmenes y núcleos**: ningún bloque de volumen donde `d_f < K`; el desvanecido llega a cero
   justo en la línea de resguardo; los términos de núcleo valen cero hasta K y su pendiente nunca
   pasa `max(pendiente propia, s_c)` (proptest); los chunks fuera de toda banda de volumen son
   `Uniform` o de 1 bit. Un núcleo que hunde bajo un tipo seco no tiene fluido; el mismo núcleo en
   el mar tiene fluido hasta el nivel.
8. **Cotas**: las cotas locales y las de WG4 cubren todo bloque que no es aire (proptest cerca de
   bordes, volúmenes, features, estructuras y planos). El camino hondo se toma solo bajo toda regla
   de superficie y todo corte. El chequeo de spans está encendido en todo test del motor.
9. **Lejanía**, por el harness exportado: celdas gruesas contra la regla de la celda en los niveles
   1 a 4, en nodos de borde de 2 y 3 tipos. Una divisoria entre dos valles cortados dentro de una
   celda deja el tope grueso sobre el real. Bandas conservadoras y tramos de volumen dentro de su
   cota.
10. **Guardado**: ida y vuelta de `boxel:worldgen`; `FORMAT_VERSION` sin cambios; un mundo sin la
    clave abre como legado con su generador viejo; una versión de entrada cambiada, un reino
    agregado y un mod quitado dan el `WorldgenDiff` esperado.
11. **Reinos**: `realm_of` coincide en cada plano y alrededor; a un chunk lo genera exactamente un
    reino; las huellas del `ContactField` coinciden desde los dos lados de un plano; el gradiente
    del relleno no deja aire en un plano de roca con roca. El chequeo rechaza dos reinos `Draw` en
    una dimensión, una forma que no está hecha, un hueco entre reinos y un plano fuera del mundo.
12. **Cielo** (EW-REALMSKY, EW-SKYUNDER): un reino de techo deja igual todo techo de cielo de la
    superficie; caminar desde el comienzo local da lo mismo que desde el tope global cuando no hay
    nada más del reino arriba; los pisos no dependen de las ediciones ni del orden de carga; un
    reino `Closed` no tiene cielo directo a ninguna profundidad; una aguja de 12 km deja igual los
    techos de la llanura y sus laderas con cielo; el fondo de un agujero a 4 km tiene cielo directo;
    bajo un techo, el aire entre el plano y la cara de abajo tiene cielo directo y las cuevas del
    techo no.
13. **Streaming y techo de la lejanía** (WG4): `known_air(pos)` nunca salta un chunk con algo; con
    un reino que sube para siempre, la lejanía quiere las mismas capas que sin él; la fuente del
    reino dibujado nunca emite una capa de otro reino; con un generador de un solo reino cuya cota
    es `vertical_chunks`, el streaming y la lejanía quieren los mismos chunks y capas con la cota y
    sin ella.
14. **Goldens** por versión de generador: `WorldGenerator::version` y `LodSource::version` suben con
    ellos.

## Consecuencias

- Los generadores de hoy no cambian: los métodos nuevos tienen valor por defecto. `FlatGenerator`
  y los generadores del demo siguen igual, y el demo queda congelado por sus goldens.
- El formato de guardado no cambia y los mundos viejos abren como antes.
- `boxel_worldgen` crece bastante (registros, selector, mezcla, pila de reinos, pasada de celdas,
  registro del guardado, consultas, harness). Nada de eso es zona protegida. Las partes protegidas
  (WG4, la luz por reino, la gravedad) llegan por separado, cada una con su OK.
- Todo el terreno queda con pendiente de a lo sumo 3:1 en cualquier línea a través de bordes de
  tipos y términos de núcleo, y el chequeo al abrir lo hace cumplir. Los costados de islas más
  empinados que 3:1 pueden mostrar las grietas de un píxel entre niveles del ADR 0031, aceptadas
  como en los acantilados.
- Un tipo de un mod escrito contra el Context del juego depende del crate del juego. Otro juego
  escribe su propio Context.
- Un `floor` por reino: vacío y suelo no comparten reino. El agujero conserva un piso de suelo, y
  las islas del cielo van en su propio reino `Void`.
- Un solo reino dibujado: la cara de abajo del techo aparece recién cuando cargan sus chunks (256
  bloques con el radio vertical por defecto), y una aguja se pierde en la bruma en el plano.
  Aceptado como "fuera de la vista".
- Con la lejanía apagada en el Nether, su niebla tiene que quedar más corta que el radio cargado y
  su color de cielo igual al de su niebla. Al salir, la lejanía se llena de cero. Un mar de lava son
  unos 1024 emisores de luz por chunk de superficie. V1 mide las dos cosas.
- Hasta WG4 (V3), la banda alta cuesta jobs: unos 7 de más por columna de llanura después de W2,
  unos 24 000 para cargar r32. `perf-smoke` y la carga a r16 deciden si WG4 se adelanta.
- La luz por reino cambia el contrato de la luz: los pisos pasan a ser por columna y por reino,
  medidos desde una cota de abajo del suelo, y un reino de techo se ilumina desde abajo. Puede haber
  errores de luz sutiles; los contienen los tests de cielo y los OK del dueño. Sin EW-REALMSKY no
  puede salir ningún reino sobre la superficie ni ningún tipo más alto que el límite de la caminata,
  y sin EW-SKYUNDER la cara de abajo del techo queda negra.
- Una entrada nueva de un mod cambia el mundo donde gana. El digest lo informa y el rendezvous lo
  limita.

## Alternativas descartadas

- **El Nether como dimensión aparte con portales.** Pide ticks y reloj por dimensión (cambiar el
  formato con un lector de la versión 1) y una pantalla de carga, y `ChunkMap` carga una
  dimensión a la vez. Los reinos apilados en y evitan todo eso: un store, un reloj, los ticks
  siguen andando, sin pantalla de carga. Y el dueño quiere llegar cavando.
- **Dos dimensiones cargadas a la vez.** Nada de esto lo necesita.
- **Mezclar a través del plano entre dos reinos.** El plano es un corte duro y la continuidad la
  acuerda el contenido (relleno graduado, aire, `ContactField`).
- **Una `LodSource` por tipo.** Desde el nivel 3 un nodo cruza bordes de tipos.
- **El DSL de nodos del diseño A en tiempo de ejecución, con su compilador `SurfaceProgram`.**
  Quedan los combinadores estáticos. Se revisa solo si #115 elige mods en archivos de datos.
- **Reglas `Point` con varias entradas con peso.** Elegían por punto de retícula: confeti de 256
  bloques.
- **Un tope de 4 tipos por columna.** Con la cota de `2^51`, `i64` alcanza para cualquier cantidad.
- **El chequeo de pendiente `|edge_a - edge_b| / 768`.** Estaba mal: dos tipos con el mismo rango de
  relieve pueden diferir en todo el rango, y solo miraba los ejes.
- **Un chequeo de pendiente solo por pares.** No cubre los puntos triples: un tipo empinado entre
  dos tipos lejanos en altura pasa cada par y rompe el tope (sección 6).
- **Un producto por rampa en los términos de núcleo.** No respeta el tope: un término de 12 km por
  una rampa de 512 bloques da una pendiente cerca de 23.
- **El desvanecido de volúmenes `(1 - Wcore)` por conteo de caja.** Podía valer 1/49 en el corte y
  pedía un rango de sesgo 49 veces la densidad más alta. La rampa por distancia no tiene ese mínimo.
- **Llenar el mar con la altura final.** El agujero se volvía un lago de 4 km. La regla de fluido
  lee la altura antes de los términos de núcleo.
- **Sin tabla de biomas en el motor** (lo que decía el plan anterior). El motor da `BiomeRule` como
  mecanismo, y el juego pone los datos.
- **Subir `FORMAT_VERSION`.** El store no tiene lector viejo: rechazaría toda partida guardada.
- **Políticas de guardado más duras**: no abrir el mundo, o dejar elegir versiones viejas de las
  entradas. El dueño eligió avisar y seguir.
- **El piso del cielo en `S - límite` por columna.** Oscurece laderas y fondos de agujeros.
  Empezar la caminata por columna, sin cambiar el piso, ahorra trabajo pero no arregla ningún caso.
- **El reino del techo `Closed`.** El aire entre el plano y la cara de abajo quedaba oscuro, y el
  contacto entre una aguja y una montaña invertida se veía negro. Por eso existe `Under`.
- **Dibujar otro reino en la lejanía (EW-REALMLOD).** Pide un árbol por reino y una paleta de luz
  por nodo. No planeado.

## Pendiente

- Se mide más adelante: las caminatas más largas en columnas de aguja y de agujero (V3), el llenado
  de la lejanía al salir del Nether (V1), y si `DistanceFog` se puede cambiar en tiempo de ejecución
  (V0), que decide si existe EW-REALMENV.
- EW-SKYUNDER es un diseño chequeado contra el contrato de la caminata (`sky_tile.rs`), no contra el
  código de las cajas de luz. Su tamaño (S a M) es una estimación.
- Del juego, sobre imágenes: la escala y el clima (W0), el tamaño de región (W0b, 2^12 para
  empezar) y qué formas de relieve de W2 pasan a ser tipos con borde (por defecto, ninguna).
- La forma de la distancia del `ContactField` (cuadrado de Chebyshev de 5,6 km o disco redondo de
  unos 7,9 km) se elige en V3, sobre imágenes de las agujas.
