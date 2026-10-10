# ADR 0028: texturas animadas de los resource packs

- Estado: aceptado (issue #45: animación y mips de las hojas)
- Fecha: 2026-10-10
- Revisa: ADR 0025, sección 5 (la textura de flujo de un pack animado no se desplaza)

## Contexto

- Con un resource pack, el agua, la lava, el sea lantern y cualquier textura con `.png.mcmeta`
  mostraban solo su primer cuadro. `boxel_pack` ya leía el `.mcmeta` (`frametime`, `frames`,
  `interpolate`) y partía la tira en cuadros, pero nadie los usaba.
- Las capas del arreglo de texturas de bloque llevan el tinte de bioma, el overlay del pasto, el
  cuarto de la textura de flujo y el alfa ya horneados (ADR 0018 y 0025), con su cadena de mips
  calculada en la CPU. Los tres tiers muestrean el mismo arreglo y deben dar la misma imagen. El
  tier Low tiene un solo bloque de uniforms y una sola textura con su sampler.
- Las hojas de BlockPixel se ven como puntos sueltos a distancia: cubren menos de la mitad de sus
  texeles y el alfa binario con umbral 0,5 las borra en los mips chicos.

## Decisión

### 1. Las reglas de Minecraft, en `boxel_pack`

`Animation::timeline(frame_count)` arma la secuencia como `SpriteContents.createAnimatedTexture`:

- Sin lista `frames`: todos los cuadros de la tira en orden, `frametime` ticks cada uno.
- Con lista: su orden y sus tiempos. Un índice suelto, o un cuadro sin `time`, dura `frametime`.
  Se descartan, con aviso, los cuadros con un índice fuera de la tira o con duración 0.
- Si queda uno o ninguno, la textura es quieta y muestra el cuadro 0 de la tira (no el primero de
  la lista), como en Minecraft. Un `frametime` 0 sin lista también la deja quieta.
- El tamaño de cuadro sigue la regla de Minecraft: los dos lados del `.mcmeta` si están, uno de
  ellos con el otro lado de la imagen, o cuadrados del lado más corto.

`Timeline::at(tick)` da el cuadro actual, el siguiente, los ticks ya pasados en el actual y su
duración. El ticker de Minecraft arranca en el primer cuadro y avanza un tick por tick de
cliente, así que es una función pura de `tick % period`, en `u64`: exacta lejos de cero, sin
saltos al envolver.

La interpolación copia `uploadInterpolatedFrame`: peso `d = 1 - sub / time`, cada canal de color
`(int) (d * a + (1 - d) * b)` en `f64` (truncado) y el alfa del cuadro actual
(`mix_channel`, `mix_pixel`). Minecraft mezcla cada nivel de mip de los dos cuadros; acá también.

### 2. Una capa terminada por cuadro

- Al cargar el pack, cada capa cuya textura está animada guarda la capa terminada (tinte,
  overlays, cuarto de flujo, alfa y toda la cadena de mips, igual que en el arreglo) de cada
  índice de la tira que su secuencia muestra (`LayerAnimation`). El arreglo arranca con el cuadro
  del tick 0.
- La capa de un cuadro interpolado se calcula al subirla: la mezcla de las dos capas guardadas,
  nivel por nivel. No hay que recalcular mips (el filtro con `powf` costaría milisegundos en una
  capa de 512 px).
- Una capa compone una sola animación: si dos partes de una capa están animadas (no pasa en
  vanilla), se mueve la primera y la otra queda en su cuadro del tick 0, con aviso.
- Tope de memoria `MAX_ANIMATION_BYTES` = 64 MiB para todas las capas animadas: una capa que no
  entra queda quieta, con aviso. Un pack de 16 px usa unos KB por capa; uno de 512 px con 32
  cuadros, unos 45 MB por capa.
- `TextureReport` cuenta las capas animadas y sus bytes; los ejemplos lo muestran en la línea de
  `textures`.

### 3. Subida por la cola, con presupuesto

- `VoxelRenderer::render` empieza mirando cada capa animada: si su clave a `texture_tick` (cuadro,
  o par de cuadros con su peso) no es la que tiene la GPU, la escribe con `queue.write_texture`,
  un nivel de mip por llamada, sobre la capa del arreglo. Las claves iguales no se suben: una
  animación sin interpolar sube una capa por cambio de cuadro, una interpolada una por tick.
- Presupuesto de bytes por frame (`set_texture_upload_budget`, por defecto
  `DEFAULT_TEXTURE_UPLOAD_BUDGET` = 1 MiB). Siempre pasa al menos una capa. Las que no entran
  quedan con el cuadro anterior y van primero en el frame siguiente (un cursor que gira). Como los
  cuadros cambian a lo sumo 20 veces por segundo, 1 MiB alcanza para todas las capas de un pack de
  hasta 128 px en un frame, y una de 512 px por frame.
- La GPU arranca sin clave conocida: el primer frame sube todas las capas animadas. Así el
  renderer no supone en qué tick se armó el arreglo, y un cambio de tier o una pérdida de
  dispositivo (que rehacen el renderer con el mismo arreglo) quedan bien solos.
- `RenderStats` suma las capas y los bytes subidos y cuenta las capas atrasadas del último frame
  (`texture_layer_uploads`, `texture_upload_bytes`, `textures_behind`). El span
  `render.textures` mide el trabajo.

### 4. El reloj

- `ViewParams::texture_tick`: ticks de juego, 20 por segundo, sin envolver (cada animación lo
  envuelve en su período, exacto). `texture_tick(seconds)` lo calcula.
- El plugin de render lo toma del tiempo transcurrido de la app (`Time`).
- `snapshot --fluid-time S` fija también este tick (`floor(20 S)`), y el ejemplo sube todas las
  capas en el primer frame del disparo (presupuesto sin tope): las capturas no dependen del
  presupuesto ni del orden.

### 5. Tiers y paridad

No cambian los ids de capa, el sampler, los bind groups ni el muestreo: solo los texeles de las
capas animadas. Los tres tiers usan el mismo camino de subida y dan la misma imagen. Low sigue con
una textura, un sampler y un bloque de uniforms.

### 6. El flujo de un pack animado no se desplaza

El ADR 0025 desplaza la textura de flujo en el shader porque la procedural es quieta. La de un
pack (`water_flow`, `lava_flow`) ya muestra el fluido corriendo con sus cuadros, como en Minecraft:
con las dos cosas el fluido corría al doble y los saltos de cuadro se veían borrosos.
`BlockTextureData::animated_flow` dice qué capas de flujo están animadas y el renderer lo pasa en
`ViewUniform::viewport.w`, que estaba libre (bit 0 agua, bit 1 lava). `flow_scroll` da 0 para
ellas. Con texturas procedurales, o un flujo quieto, todo sigue igual.

### 7. Mips de recorte que conservan la cobertura

Las capas de recorte (hojas, plantas, antorcha) guardan alfa 0 o 255 en cada mip. Antes el umbral
era 0,5 en todos los niveles. Ahora un mip guarda al menos la fracción opaca del nivel 0: si con
0,5 quedan menos texeles, pasan los de más cobertura, con los empates repartidos en orden de Bayer,
y nunca uno de cobertura 0. Las hojas que cubren menos de la mitad ya no se deshacen a distancia.
Las que conservan su cobertura con 0,5 (las hojas y el pasto procedurales) quedan igual; la amapola
y la antorcha ganan uno o dos texeles en sus mips chicos.

## Alternativas descartadas

- **Cada cuadro como capa del arreglo, y el shader elige.** No hay subidas por frame, pero el
  arreglo crece con todos los cuadros (un pack con mucha animación pasa el límite de capas de
  algunos dispositivos), el shader necesita una tabla de capa a cuadro (en Low, otro bloque de
  uniforms o más datos en el único que hay) y la interpolación de Minecraft pide mezclar dos
  muestras en el shader para todos los quads, animados o no.
- **Mips en la GPU** (un pase que reduce la capa): un pipeline más por tier, distinto en Low, y el
  filtro con alfa de la CPU habría que copiarlo en WGSL con la misma aritmética para la paridad.
- **Recalcular la capa en cada tick** (componer, teñir y hacer mips): simple, pero cuesta
  milisegundos en packs de 256 o 512 px. Guardar los cuadros terminados lo evita a cambio de
  memoria, que tiene tope.
- **Usar `fluid_time` como reloj.** Envuelve cada 60 s en `f32`, y los períodos de los packs (64
  ticks el agua de vanilla) no dividen 1200 ticks: habría un salto cada minuto.

## Consecuencias

- Agua, lava, sea lantern y cualquier capa animada de un pack se mueven como en Minecraft, en los
  tres tiers. Sin pack no cambia nada (no hay capas animadas).
- Costo con Whimscape (16 px): 6 capas animadas, 112 KB de cuadros guardados. Agua y lava
  (quietas y de flujo) y el sea lantern interpolan, así que suben en cada tick; la antorcha cada 2
  ticks. Son unas 6 capas de 1,4 KB por tick, 5 llamadas a `write_texture` por capa. Con
  BlockPixel, 5 capas y 124 KB (su `water_still` lista el índice 20 en una tira de 20 cuadros: se
  descarta con aviso, como en Minecraft). Ver la medición.
- `textures.rs` cambió el mip de las capas de recorte para todos, no solo para los packs.

## Pruebas

- `boxel_pack`: tamaño de cuadro, secuencia por defecto, lista explícita con repeticiones, ticks
  lejanos, cuadros inválidos descartados, un cuadro solo es quieto, la interpolación de Minecraft
  en todos los bytes y pesos.
- `boxel_render::texture_animation`: claves y píxeles por tick, un cuadro repetido no se mezcla
  consigo mismo, el presupuesto (al menos una capa, las salteadas primero, sin subir lo que no
  cambió, sin tope).
- `pack_textures`: un pack generado con agua interpolada, lava con lista, flujo de agua con dos
  cuadros (cada uno con su cuarto) y un sea lantern inválido que queda quieto.
- `textures`: los mips de recorte conservan la cobertura, los empates se reparten, una zona
  transparente sigue transparente y las hojas procedurales no cambian.
- GPU (`gpu_animated_textures`): una piedra animada roja, verde y azul en los tres tiers y en
  todos los adaptadores, a varios ticks (incluido un mip chico visto de lejos, la mezcla
  interpolada, la vuelta del ciclo y un presupuesto de un byte), con la misma imagen en todos los
  tiers. Otro test mira el costado de una fuente de agua: con un flujo quieto se desplaza, con uno
  animado no.

## Medición (nivel 1)

- Sin ruido: con Whimscape, 6 capas animadas suben 110 capas por segundo (5 capas interpoladas
  a 20 por segundo y la antorcha a 10), unos 150 KB por segundo y 550 llamadas a
  `write_texture`. Sin pack no se sube nada.
- `just perf-smoke surface -- --pack assets/Whimscape` (RTX, r16, sin tope, una corrida por
  lado contra `main`): 1941 a 1939 fps (-0,1 %), p99 de 1,25 a 1,33 ms, 0 frames lentos en los
  dos lados. Veredicto ok. No aparecieron los frames fijos en 33 ms de la issue.

## Pendiente de la issue #45

- Rotaciones de variante, UV por cara y modelos al azar: hacen falta con los bloques especiales
  (#38). Los bloques de serie son cubos completos y alcanzan.
- Tinte por bioma: depende de los biomas (#43). Sigue fijo (llanura).
- Las estrategias de mip del `.mcmeta` nuevo (`texture.mipmap_strategy`) se ignoran.
- Dos partes animadas en una misma capa: se mueve solo la primera.
