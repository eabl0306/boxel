# ADR 0030: niebla y nubes cúbicas, un plugin opcional

- Estado: aceptado (issue #47), revisado el mismo día tras la primera y la segunda prueba del
  dueño (al final)
- Fecha: 2026-10-10

## Contexto

La issue #47 pide niebla y nubes con forma de cubos que se mueven con el viento. La visibilidad
es acumulativa: mientras más niebla cruza la mirada, menos se ve. Hay capas a distintas alturas
(bancos bajos, niebla de valle, nubes) y no hay interacción con mobs, físicas ni jugabilidad.

El dueño decidió (comentario de la issue, 2026-10-10):

- La niebla es opcional: un plugin aparte que el juego agrega solo si la quiere.
- Sin el plugin no hay pasada extra ni costo alguno.
- El estilo (tamaño de cubo, capas, colores, densidades, viento) es configuración del juego. Los
  presets del ejemplo van en `boxel_demo` (ADR 0029).

Restricciones del motor que pesan aquí:

- El tier Low en GL tiene un solo uniform block y una sola unidad de textura (ADR 0011).
- Las imágenes deben salir iguales en High, Mid y Low.
- Las coordenadas del mundo son `i64` y todo debe ser exacto lejos del origen (ADR 0014).
- La meta de 144 fps en la Radeon 780M no se negocia para el motor. La niebla, que es opcional,
  no la tiene (sección 6).

## Decisión

### 1. Un plugin aparte, sin costo cuando no está

`FogPlugin` (`boxel_render::fog`, en el prelude) no está en `DefaultPlugins`. Al agregarlo:

- inserta `FogSettings` (las capas y cómo se dibujan) y `FogClock` (el reloj del viento);
- avanza el reloj en `FixedUpdate`, un tick por paso fijo (64 por segundo);
- en `RenderSystems::Extract` copia los ajustes y el tick al frame. Valida los ajustes solo
  cuando cambian. Un cambio inválido se avisa una vez en el log y quedan los últimos válidos.

Sin el plugin el frame no trae niebla y el renderer no crea nada de ella: ni pipelines, ni
target, ni módulo de shader. La profundidad principal conserva su uso de siempre, sin
`TEXTURE_BINDING`. Un test de GPU lo comprueba con imágenes idénticas byte a byte.

Las apps sin ECS (tests, `snapshot`) usan `VoxelRenderer::set_fog(device, Some(FogInput))`.

Con el plugin, el smoke test de la GPU también dibuja una capa densa y lee el píxel. Si falla, la
niebla queda apagada en ese dispositivo con un aviso. Nunca cambia el tier.

### 2. El crate `boxel_fog`

Es CPU puro y datos, sin GPU ni ECS:

- `config`: `FogLayer`, `FogNoise`, `NoiseDims`, `FogFade`, `FogResolution`, `validate` y
  `density_for_visibility`.
- `noise`: el ruido entero, el mismo bit a bit que el shader, y una referencia directa en `i128`.
- `layer`: una capa reducida a enteros chicos relativos al origen de render (`LayerParams`), el
  momento del reloj (`FogTime`) y los rayos de la cámara (`RayBasis`).
- `march`: la marcha del shader en `f32`, la referencia de los tests de GPU.
- `pack`: cómo la marcha guarda su resultado en un texel.

Existe porque los presets de `boxel_demo` no pueden usar tipos de `boxel_render`: el render usa
`boxel_demo` en sus tests, y un tipo duplicado rompería esos tests. `tools/no_content.py` permite
que `boxel_demo` dependa de `boxel_fog`.

### 3. Cómo se dibuja

Una marcha de rayos a pantalla completa, igual en los tres tiers. Corre dentro de
`VoxelRenderer::render`, después de todo el pase principal (opaco, cutout, contorno y
translúcido) y antes de los overlays y el blit.

**Pasada 1, la marcha.** Escribe en un target `Rg32Uint`, a media resolución por defecto (un rayo
cada 2x2 píxeles) o completa.

- `x`: el color de la niebla por unidad de opacidad, en 11-11-10 bits con raíz cuadrada.
- `y`: cuatro bytes de opacidad, uno por píxel del bloque de 2x2. Cada píxel usa su propia
  profundidad, así que los bordes del terreno quedan nítidos aunque el rayo sea uno solo.

Por cada capa, el rayo recorre sus celdas con un DDA (Amanatides y Woo) en `f32` relativo a la
cámara, hasta la profundidad opaca de sus píxeles. Las bandas de las capas no se solapan, así que
ordenadas por altura se recorren de adelante hacia atrás. En cada celda:

- la cobertura sale del ruido entero (sección 4) y la extinción es `densidad * cobertura`. Cerca
  del piso y del techo de la banda los bordes la afinan (sección 4), así que la capa no termina
  en un plano;
- la transmitancia baja `exp(-extinción * largo)`;
- el color es el de la capa iluminada, por el brillo de su altura en la banda (`height_shade`,
  apagado por defecto), desvanecido hacia el color del cielo con la distancia.

La niebla no tiene caras. Una capa puede pedir el sombreado de la cara por la que el rayo entra
al cuerpo de celdas (`face_shade`, por ejemplo `VOXEL_FACE_SHADE`, el de los vóxeles), pero por
defecto todo vale 1.

**Niebla suave.** Cerca de la cámara (`soft_distance`, de 8 a 16 bloques en los presets) la
niebla no muestra bordes de celdas. Cada celda que empieza ahí se corta en trozos de un bloque
(a lo sumo 16 por celda). Cada trozo mezcla la cobertura de la celda con la del ruido leído en el
medio del trozo: toda del ruido hasta `soft_distance` y de vuelta a la celda en la mitad de esa
distancia más. El ruido de una celda es el de su esquina, así que la niebla suave lo lee medio
cubo atrás: el medio de una celda ve el valor de su celda. Los trozos no cuentan como pasos;
`soft_distance` va de 0 (apagada, por defecto) a 32 bloques, así que el trabajo extra tiene tope.

Para no gastar de más:

- sale antes cuando la profundidad óptica pasa de 6 (ya no se ve nada detrás);
- tiene un tope de celdas por rayo (`max_steps`). Pasado el tope, el resto de cada capa usa su
  densidad media a cada altura, sin un muro visible donde se corta. Cerca de los bordes esa media
  baja como bajan las celdas, así el resto no pinta una franja plana donde las celdas ya
  terminaron. Desde el ADR 0032 es el perfil de 8 nudos de la bruma lejana (`HazeProfile`); antes
  era `MeanCoverage`, una tabla de la media con el umbral subido un cuarto, la mitad y tres
  cuartos;
- cruza de un paso un tramo entero de la grilla del ruido cuando sus esquinas no dejan cobertura
  ni con el umbral más bajo de las filas del tramo (la cota es conservadora: el ruido de valor
  nunca sale del rango de sus esquinas, y el umbral de las filas sale de la subida en la fila más
  baja y del borde más débil, menos 1 por los saltos del smoothstep). Con bordes y subida salta
  más tramos, nunca uno con cobertura. En la niebla suave no lo hace: lee puntos de hasta medio
  cubo fuera del tramo;
- toma de un paso una capa cuyas celdas están todas llenas.

**Pasada 2, la composición.** Un triángulo a pantalla completa sobre el color principal, con
mezcla `SrcAlpha, OneMinusSrcAlpha` solo en el color. Descarta los píxeles con opacidad 0, así
que un píxel sin niebla no cambia. La interfaz nunca tiene niebla y las capturas sí la tienen.

**Layout para Low.** Cada pipeline tiene a lo sumo un uniform block y una textura, sin sampler:

- la marcha lee la profundidad como `texture_2d<f32>` no filtrable con `textureLoad` (grupo 0)
  y el uniform `Fog` (grupo 1);
- la composición lee solo el target de la marcha (grupo 0).

**Datos por frame.** El uniform `Fog` tiene un encabezado de 112 bytes y 4 capas de 272, 1200
bytes en total (224 y 1008 antes del ADR 0032, 256 y 1136 antes de su M2), y se escribe cada
frame. No se sube ninguna textura ni volumen: la densidad es una
función de la celda absoluta, el momento del reloj y la capa.

### 4. Ruido entero y viento

La CPU hace la cuenta absoluta en `i64` e `i128` y le pasa a la GPU solo enteros chicos relativos
a la celda del origen de render: el desplazamiento dentro del paso de la grilla, el punto de la
grilla en dos palabras de 32 bits (baja y alta) y la fracción del viento en 12 bits.

- **Hash.** Por eje, la palabra alta se pliega en la baja (`baja ^ alta * 0x9e3779b1`). Las tres
  palabras pasan por una cadena de `lowbias32` desde la semilla de la octava. El valor son los 12
  bits altos.
- **Interpolación.** Un smoothstep en punto fijo y lerps enteros, trilineales (`Volume`) o
  bilineales (`Flat`), con cada producto por debajo de 2^26. Se puede leer en cualquier punto,
  en 1/4096 de celda: la celda lo lee en su esquina y la niebla suave donde haga falta.
- **Octavas.** Una o dos. La segunda tiene la mitad de escala y un tercio del peso.
- **Cobertura.** Una rampa `clamp(ruido + 1 - umbral) * ganancia >> 8`, de 0 a 4096, pasada por
  el mismo smoothstep entero. Una celda que cruza el umbral nace casi transparente y se llena, y
  una que muere se apaga despacio. Con suavidad 0 los cubos son llenos o vacíos y aparecen de
  golpe.
- **Perfil.** El umbral sube con el cuadrado de la altura en la banda (`threshold_rise`), por
  fila de celdas y en enteros. La base queda ancha y plana y los topes, finos y desparejos.
- **Bordes.** `edge_bottom` y `edge_top`, en bloques (0, por defecto, corta en el plano como
  antes). Dentro de un borde el umbral sube por el smoothstep entero hasta "nunca cubierto" en
  el plano: `umbral + (4096 - umbral) * smooth(1 - d / borde)`. Cerca del plano solo se llenan
  las celdas de ruido fuerte, así que cada columna termina a su propia altura: un perfil de
  cubos desparejo en vez de un plano, y con suavidad se desvanece en vez de cortarse. Una fila
  de celdas mide `d` desde su cara más cercana al plano, así que la fila pegada al plano siempre
  queda vacía. La niebla suave lo mide en cada punto, medio cubo hacia adentro (como lee el
  ruido), así que el medio de una celda tiene exactamente el borde de su fila y el último medio
  cubo antes del plano no tiene niebla. Las cuentas son enteras por fila y, en la niebla suave,
  en 1/4096 de bloque, iguales en la CPU y en los tres tiers; lejos del origen dan lo mismo
  porque usan las filas de la banda relativas a la celda del origen.
- **Viento.** El momento es `FogTime`: `tick` (`u64`, 64 por segundo) y `subtick`, la fracción
  del tick siguiente en 1/65536, nunca segundos en `f32`. `FogClock` toma `subtick` del sobrante
  del paso fijo en cada frame, así el viento se mueve en cada frame y no a saltos de 64 por
  segundo. El desplazamiento es `viento * momento` en q16: celdas enteras más una fracción de 12
  bits. Las celdas no se mueven; cambia cuáles están llenas.

La cobertura sale idéntica en el oráculo de CPU, en los tres tiers, en todos los backends y a
cualquier distancia del origen, también en puntos dentro de las celdas y entre ticks.

### 5. El estilo es del juego

Cada `FogLayer` es un dato:

- banda `[min_y, max_y)`, múltiplos del alto de celda, sin solaparse con otras capas;
- celda `[x, y, z]` de 1 a 32 bloques (por ejemplo `[2, 2, 2]` o `[12, 4, 12]`);
- ruido: escala (potencia de dos de 1 a 64), octavas, `Flat` o `Volume`, semilla;
- umbral, cuánto sube hasta el tope de la banda, bordes abajo y arriba (en bloques, hasta 1024) y
  suavidad;
- densidad por bloque (`density_for_visibility(V) = 3 / V`: se ve un 5 % a V bloques);
- color, `sky_tint` (cuánto toma del color del cielo, el tono del atardecer), brillo por altura
  y sombreado por cara (los dos apagados por defecto);
- distancia de la niebla suave (de 0 a 32 bloques, apagada por defecto);
- viento en bloques por segundo;
- distancia máxima de las celdas (hasta 4096 de sus celdas más chicas y 65.536 bloques; hasta
  el ADR 0032 era 4096 bloques), la bruma que sigue después (`haze`, 0 por defecto, ADR 0032) y
  desvanecido (`Scene`, como el terreno, o un rango propio).
  Una capa `Scene` que termina antes que el terreno se desvanece con la regla de la niebla de
  distancia en su propio final, para no cortar en seco donde el terreno todavía se ve claro (el
  valle del ejemplo, de 384 bloques, a 16 chunks). Con la niebla de distancia apagada, `Scene`
  no desvanece y la capa termina de golpe, como el terreno. Una capa `Scene` con bruma toma en
  cambio el desvanecido del terreno más lejano, el del LOD mientras dibuja (ADR 0032).

`FogSettings` agrega `enabled`, hasta 4 capas, la resolución (media por defecto) y `max_steps`
(64 por defecto, `DEFAULT_MAX_STEPS`). Se cambia en tiempo de ejecución con `ResMut<FogSettings>`.

```rust
app.add_plugins(FogPlugin::new(vec![boxel_demo::fog::valley(), boxel_demo::fog::clouds()]));
```

Los presets del ejemplo están en `boxel_demo::fog`, todos suaves (sin caras, con rampa suave y
niebla suave cerca):

- `clouds()`: nubes volumétricas, cubos de 8 bloques entre y 176 y 240 con ruido 3D (casi todas
  entre 192 y 224), bordes de 20 abajo y 28 arriba, una subida suave del umbral, base gris y
  tope claro;
- `valley()`: bancos de cubos de 2 bloques entre 46 y 70, sobre el mar, más densos de 50 a 58,
  con un borde de 12 arriba (la mitad de la banda) y de 4 abajo. La fila de 46 a 48 siempre
  queda vacía, así que no hay niebla bajo la superficie del mar (47);
- `blanket()`: un manto, el peor caso, con bordes de un cuarto de su alto arriba y un octavo
  abajo (a lo sumo 16 y 4);
- `sunset()`: valle y nubes con más color del cielo.

### 6. Valores por defecto y costo medido

La niebla es para GPUs con margen. El dueño decidió (2026-10-10) que la Radeon 780M solo tiene
que mover bien el motor: no tiene que entrar en un frame de 144 Hz con la niebla prendida. Por
eso los valores por defecto se eligieron por cómo se ven y por su costo en la RTX 5050:

- media resolución;
- 64 celdas por rayo (`DEFAULT_MAX_STEPS`).

`just perf-smoke` del 2026-10-10 sobre 62b83e5, con los presets suaves y las nubes volumétricas:
el mismo binario con la niebla apagada y con `--fog both` (valle y nubes, el preset típico), 10 s
por lado, radio 16, 1280x720, a las 12:00. La superficie es el overworld volando a 64 bloques por
segundo; el bosque vuela a 32. El harness repitió cada par y dio lo mismo.

| caso | fps | p50 ms | p99 ms | frames > 6.94 ms |
|---|---:|---:|---:|---:|
| RTX, superficie, sin tope | 1939 -> 947 | 0.48 -> 0.94 | 1.40 -> 2.72 | 0 -> 0 |
| RTX, bosque, sin tope | 3903 -> 1146 | 0.23 -> 0.78 | 0.68 -> 2.34 | 0 -> 0 |

En la RTX la niebla cuesta ahora de 0,46 a 0,55 ms por frame en la mediana y ningún frame pasa de
6,94 ms. Con los bordes de la segunda prueba subió a 1,0 a 1,1 ms (al final). Antes de los cambios (bac728d, nubes planas de una octava, sin niebla suave) costaba de
0,26 a 0,32 ms. Lo más probable es que la diferencia venga de las nubes: ruido 3D de dos octavas,
casi sin tramos vacíos que saltar, y una banda de 32 bloques en vez de 4. Sobre bac728d la 780M
pasaba de 550 a 359 fps sin tope (unos 0,8 ms en la mediana, de 9 a 12 frames lentos cada
10 s); no se volvió a medir.

No se ajustó para que entre: queda en una issue de prioridad baja, con estas perillas (todas del
juego):

- `max_steps`: en mediciones previas de la rama, con una banda de valle de 24 bloques, la 780M
  pasó de unos 150 frames lentos cada 10 s con 64 celdas a 92 con 32 y a 15 con 16;
- celdas más grandes (de 4 bloques en vez de 2, la mitad de celdas por rayo);
- una banda más fina o una `max_distance` más corta;
- una octava en vez de dos: con una la marcha salta los tramos vacíos de la grilla del ruido,
  con dos casi nunca puede;
- la resolución: la media ya es la más barata; un cuarto de resolución sería nuevo.

## Límites conocidos

- **Translúcidos.** La marcha llega hasta la profundidad opaca. La niebla que queda detrás de un
  vidrio o bajo el agua se pinta encima de esa superficie, y la niebla vista desde abajo del agua
  no toma su color. El valle del ejemplo empieza en y 48, encima del agua del mar (termina en 47),
  así que el mar sale exacto. Una banda que baja bajo el agua o queda detrás de un vidrio no.
- **Sin mapa del suelo.** Las bandas son planas: la niebla también llena las cuevas y los aleros
  dentro de su banda (el terreno las tapa, salvo en las entradas) y no se junta en los valles que
  quedan arriba de la banda. Un borrador del mapa del suelo, hecho con los techos de cielo de la
  luz (ADR 0015) sin leer bloques, quedó en la rama `wip/fog-ground` sin conectar al shader.
- **Media resolución.** Los bordes de los cubos avanzan de a 2 píxeles y el color es la media del
  bloque de 2x2. La resolución completa está disponible y cuesta cerca de cuatro veces más.
- **Tope de celdas.** Pasadas las 64 celdas de un rayo (unos 80 a 120 bloques en cubos de 2), el
  resto de la capa es una bruma de densidad media. Con menos celdas la bruma empieza antes.
- **Cubos de canto.** Si un plano de celdas pasa cerca del ojo y casi paralelo a la mirada, las
  dos filas de celdas a cada lado se ven distintas desde donde termina la niebla suave hasta el
  horizonte: una línea recta en la imagen. Es el aspecto cúbico visto de canto; sin la niebla
  suave también estaba.
- **Desde la altura del tope.** Con el ojo a la altura del techo de una capa, toda la niebla
  queda debajo del horizonte, y la lejana se acumula en una franja que llega casi al horizonte:
  es la geometría de mirar una capa de canto. Los bordes la vuelven un degradé desparejo cerca y
  el desvanecido la funde con el cielo lejos, pero no la borran.
- **Bordes y celdas.** La fila pegada a un plano con borde siempre queda vacía, así que un borde
  menor que una celda solo vacía esa fila y deja otro plano una fila más adentro. Los presets
  usan bordes de 2 celdas o más.
- **Profundidad muestreada.** Con el plugin, la profundidad principal se puede leer como textura.
  En algunas GPUs eso puede costar su compresión. Sin el plugin no cambia nada.

## Alternativas descartadas

- **Cubos rasterizados como instancias.** Sin salida temprana, decenas de fragmentos mezclados
  por píxel, rayas en las caras interiores y problemas con el plano cercano.
- **La niebla como bloques en los chunks.** Habría que volver a mallar todo el tiempo y sería
  translúcido caro.
- **Niebla por fragmento en los shaders de vóxeles.** Toca los shaders principales y no pone
  niebla sobre el cielo vacío.
- **Upsample bilateral.** Necesita una segunda textura, y Low no la puede ligar.
- **Partir el pase translúcido** para que la niebla detrás del agua y el vidrio sea exacta.
  Queda para después, con su propia medición.

## Cómo se probó

- `boxel_fog`: tests unitarios y proptest. El ruido partido como lo ve la GPU contra la cuenta
  directa en `i128`, con orígenes cerca de 0, ±2^31, 2^40 y el límite del mundo, en puntos dentro
  de las celdas, entre ticks, con umbral que sube y con bordes; el viento como traslación
  exacta, también medio tick; la cota de los tramos vacíos es conservadora, también con subida y
  bordes, y con ellos solo salta más; con umbral que sube, cada fila está dentro de la de abajo;
  una capa suave no cambia la cobertura de una celda más de 1,6 % por tick, y con suavidad 0 sí
  salta; invariantes del DDA; la marcha (exp(-σL) en una losa, salida temprana, la cola del tope
  sin salto, sin caras por defecto, un sombreado por cuerpo si se pide, brillo por altura, cámara
  adentro, corte en la profundidad, capas en los dos sentidos, la niebla suave igual a la
  integral fina del ruido, idéntica a la cúbica pasada su mezcla, y el medio de una celda lee su
  celda); el empaquetado; cada error de `validate`.
- `boxel_fog/tests/edges.rs`: ninguna columna llega a un plano con borde (la fila pegada nunca
  se cubre, el último medio cubo de la niebla suave tampoco, y un rayo dentro de ese medio cubo
  no cruza niebla, cúbica o suave, con tope o sin él), lejos del origen también; el medio de una
  fila tiene su borde bit a bit; las columnas terminan a muchas alturas y cada fila del borde es
  más fina que la de abajo; saltar tramos con bordes no pierde niebla; la niebla suave con bordes
  es la integral fina del ruido; la cola pasada el tope da en promedio lo mismo que las celdas
  cerca del techo.
- `boxel_demo`: ningún preset tiene caras, todos son suaves, ninguna celda cambia más de 1,6 %
  por tick y ninguno termina en un plano (bordes de 2 celdas o más arriba y abajo, el de abajo
  no más hondo que el de arriba, y un centro libre).
- `boxel_shaders/tests/naga.rs`: `fog.wgsl` valida y traduce a GLSL 330, ES 300, 430 y ES 310,
  MSL, HLSL y SPIR-V; el layout del uniform coincide con los espejos de Rust; cada entry point
  liga lo que Low admite; las constantes del shader son las de `boxel_fog`.
- `boxel_render/tests/gpu_fog.rs`, en cada tier de cada adaptador:
  - sin niebla, con `set_fog(None)`, apagada o fuera de alcance, la imagen es idéntica byte a
    byte y la profundidad no tiene `TEXTURE_BINDING`;
  - el ruido de la GPU es el oráculo bit a bit, también lejos del origen, en las filas de los
    bordes, en el peso del borde de la niebla suave a muchas alturas, dentro de las celdas,
    entre ticks y con umbral que sube;
  - cada tier dibuja la marcha de referencia (texel a texel y la composición) en nubes planas y
    volumétricas con bordes desde abajo, justo debajo, adentro y arriba, cámara adentro de un
    manto y de un banco suave, un banco con bordes visto desde la altura de su techo, un banco,
    el atardecer, una pileta con vidrio y postes finos;
  - orígenes de render a unos bloques de distancia dan la misma niebla, cúbica y suave;
  - una losa no tiene caras, salvo que la capa pida el sombreado de los vóxeles;
  - cambios de tamaño, de resolución y un renderer nuevo;
  - el smoke de la niebla pasa en cada tier.
- `just perf-smoke` en la RTX 5050 y la 780M (sección 6).

## Consecuencias

- Un juego agrega niebla y nubes con un plugin y datos. Sin el plugin, el motor es el de antes.
- `stress --fog PRESET --fog-res half|full --fog-steps N` y `snapshot --fog PRESET --fog-tick N`
  sirven para medir y para mirar.
- Pendiente:
  - el mapa del suelo, para que la niebla siga el terreno y se junte en los valles;
  - partir el pase translúcido, para la niebla detrás del agua y el vidrio;
  - un atajo en `voxel_world` para prender y apagar la niebla.

## Cambios tras la primera prueba (2026-10-10)

El dueño voló por la niebla y pidió cuatro cambios:

1. **Sin caras.** El sombreado por cara se veía como paredes y sombras del lado cercano de un
   banco, y como una sombra cúbica contra el cielo desde adentro. Ahora es opcional y está
   apagado por defecto y en todos los presets. El aspecto cúbico queda: una densidad por celda
   y bordes nítidos.
2. **Niebla suave cerca.** Dentro de `soft_distance` el ruido se lee punto a punto y no se ven
   cubos; más lejos vuelven (sección 3).
3. **Aparecer y desaparecer de a poco.** Las celdas saltaban porque la cobertura era casi
   binaria (las nubes tenían suavidad 0). Ahora la rampa pasa por un smoothstep, los presets usan
   suavidad de 0,2 a 1 y el reloj se interpola entre ticks. Por la acumulación, las celdas de
   afuera de un banco se ven finas y las de adentro densas.
4. **Nubes volumétricas.** Las nubes del ejemplo eran planas como las de Minecraft. Ahora tienen
   volumen: ruido 3D, una banda de 32 bloques, cubos de 8, umbral que sube (base plana, topes
   desparejos) y brillo por altura en vez de caras.

## Cambios tras la segunda prueba (2026-10-10)

El dueño volvió a volar por la niebla y vio dos cosas:

1. **Bases de nube planas.** Con el umbral que sube, la fila de abajo era la más llena, así que
   todas las nubes apoyaban su base en el mismo plano.
2. **Techo plano.** Cada capa llenaba su banda hasta el piso y el techo y se cortaba ahí. Con el
   ojo a la altura del techo se veía una franja blanca de borde recto, fuera cual fuera la
   densidad.

Lo que cambió:

- **Bordes** (sección 4): `edge_bottom` y `edge_top`. Cerca de un plano solo se llenan las celdas
  de ruido fuerte, así que cada columna termina a su propia altura. La fila pegada al plano
  siempre queda vacía y la niebla suave se apaga medio cubo antes. El salto de tramos vacíos y
  la cola pasada el tope los tienen en cuenta, así que tampoco ahí aparece un plano.
- **Presets.** El valle va de 46 a 70 con bordes de 4 abajo y 12 arriba: más denso de 50 a 58 y
  apoyado en el mar, sin niebla bajo el agua. Las nubes van de 176 a 240 con bordes de 20 y 28 y
  una subida de 0,2: conservan su volumen y la base queda apenas más pareja que el tope. El
  manto tiene bordes y el atardecer hereda los del valle y las nubes.
- **Imágenes** en `bench-results/fog-look/` (fuera de git), con dos tomas nuevas: el ojo a la
  altura del techo del valle y justo debajo de las nubes.

**Costo.** `just perf-smoke` del 2026-10-10 sobre 7bb443e, como en la sección 6: el mismo
binario sin niebla y con `--fog both`, RTX, radio 16, sin tope. El harness repitió cada par y
dio lo mismo.

| caso | fps | p50 ms | p99 ms | frames > 6.94 ms |
|---|---:|---:|---:|---:|
| RTX, superficie, sin tope | 2187 -> 620 | 0.41 -> 1.45 | 1.00 -> 4.36 | 0 -> 1 |
| RTX, bosque, sin tope | 3868 -> 680 | 0.23 -> 1.35 | 0.70 -> 4.25 | 0 -> 0 |

La niebla cuesta ahora de 1,0 a 1,1 ms por frame en la mediana, el doble que antes. Casi todo
viene de las nubes: su banda pasó de 32 a 64 bloques y un rayo cruza el doble de celdas (en la
marcha de referencia, de 7 a 13 pasos por rayo mirando al horizonte desde y 100). El valle, con
la cámara adentro, pasó de 29 a 37. No se ajustó. Si hace falta, las perillas son del juego: una
banda de nubes más baja (por ejemplo de 184 a 232 con bordes de 16 y 20) o cubos de nube más
altos.

## Cambios con la bruma lejana (ADR 0032, 2026-10-11)

- Una capa puede seguir pasadas sus celdas como una bruma de su densidad media a cada altura
  (`haze`), hasta el horizonte del LOD. Con `haze` en 0, el valor por defecto y el de todos los
  presets de este ADR, se ve como antes.
- El límite de `max_distance` se cuenta en celdas: 4096 de las más chicas de la capa, con un tope
  de 65.536 bloques.
- La cola pasado el tope de pasos usa el perfil de la bruma en lugar de `MeanCoverage`. Los rayos
  que agotan el presupuesto cambian un poco, también en `both`.
- El uniform crece a 1136 bytes. Los detalles están en el ADR 0032.
- La optimización del ADR 0032 (M2, 2026-10-12) deja `both` en 1,06 a 1,11 ms por frame medio y
  0,96 a 1,01 ms en la mediana (RTX, 1280x720, sin tope), unas 1,1 veces menos que antes del ADR
  0032. La marcha ya no camina la fila pegada a un plano con borde, que siempre está vacía. El
  uniform queda en 1200 bytes.
