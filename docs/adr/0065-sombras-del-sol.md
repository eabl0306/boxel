# ADR 0065: sombras del sol en High

- Estado: aceptado (issue #9, fase P8 del plan de luz). El dueño aprobó el plan
  (`docs/planes/sun-shadows-plan.md`) el 2026-11-03 y aceptó tal cual las ocho recomendaciones de
  su sección 9. Se construye por carriles (sección 6 del plan). Hechos: el S0 (este ADR), el S1
  (el gancho del sol en los shaders), el S2 (el sol y las cascadas en la CPU) y el S3 (los mapas
  de sombra). Faltan el S4 (los receptores), el S5 (celdas y modelos) y el S6 (depuración, demo y
  herramientas).
- Fecha: 2026-11-03

## Contexto

La luz del cielo está horneada en las mallas (ADR 0015). Da la luz suave de afuera, pero no la
sombra nítida del sol: un pilar al mediodía no proyecta nada en el suelo, y bajo un árbol el suelo
solo se ve un poco más oscuro por el cielo esparcido.

El plan de luz dejaba para P8 "cascadas de sombras del sol en High". El plan de sombras revisó
eso contra el código y comparó tres maneras de hacerlo.

## Decisión

### 1. La regla

- El sol es una dirección `L` (hacia el sol) que sale de `TimeOfDay` y de una inclinación del
  camino del sol.
- Cada reino tiene una fuerza de sombra `k = fuerza * sol * desvanecido`. La fuerza es 0,4 por
  defecto; `sol` es 1 si el reino tiene sol; el desvanecido sube de 0 en el horizonte a 1 con el
  seno de la elevación en 0,2 (unos 11,5 grados). De noche `k` es 0.
- La visibilidad del sol `v` (0 a 1) de un fragmento sale del mapa de sombras, por una rampa en
  `N . L` (una cara de espaldas al sol está en su propia sombra), y sube a 1 al final de la
  distancia de sombras.
- El término del cielo pasa a ser `level_curve(S) * cielo * (1 - k * (1 - v))`. La luz de bloque,
  las luces dinámicas, el piso de ambiente, el AO, el sombreado de caras, los quads emisivos y la
  niebla no cambian.

Por qué así:

- **Solo oscurece.** El factor está entre `1 - k` y 1, y todo lo que sigue (`max`, productos, la
  mezcla de la niebla) es monótono. Con sombras la imagen nunca es más clara que sin ellas, píxel
  por píxel. Las caras al sol quedan con el color de hoy.
- **Cada falla da la imagen de hoy.** Un caster que falta, una lista llena, un chunk que no llegó o
  un receptor fuera de alcance dan `v = 1`.
- **Las cuevas no reciben sol.** Con `S = 0` el término del cielo es 0, diga lo que diga `v`.
- **El cielo horneado sigue haciendo lo suave.** La sombra suma lo nítido encima. En un cuarto con
  ventana, la mancha del piso donde entra el sol queda como hoy y el resto pierde `k`.

No agrega luz: un rayo de sol por una ventana en un cuarto con cielo 5 da cielo 5 sin sombra, no
sol pleno. Un término de sol directo necesitaría su propia máscara contra las cuevas y rompería
"cada falla da la imagen de hoy".

### 2. Opción A: cascadas de vóxeles

Mapas de sombra en cascada, dibujados desde la arena de quads que ya está en la GPU, con cinco
decisiones propias de los vóxeles:

1. Las sombras solo oscurecen el término del cielo (sección 1).
2. Dan sombra las **caras de espaldas al sol**, elegidas por cubeta de cara (3 de las 6 cubetas
   opacas). La geometría de vóxeles es cerrada: queda un bloque de margen detrás de cada cara
   iluminada. No hay acné ni peter-panning, y el sesgo de profundidad casi no hace falta.
3. Solo dan sombra los chunks con alguna **cara con cielo** (un bit por chunk, de su paleta de
   luz). Bajo tierra el paso de sombras no dibuja nada.
4. La **distancia de sombras es fija** (160 bloques): el costo no crece del radio 16 al 32.
5. **Mid y Low no cambian**: ningún binding, campo de uniform, atributo de vértice ni texto de
   módulo nuevo les llega. High con las sombras apagadas es el High de hoy.

| | **A: cascadas (elegida)** | B: volumen de ocupación | C: mapa de alturas |
|---|---|---|---|
| VRAM en High | 51 MiB (12 a 64) | 6 a 28 MiB | 5,4 MiB |
| Datos nuevos del mundo | ninguno | vóxeles por chunk | techos por tile |
| RTX 5050, r16 y r32 | +0,08 a 0,15 ms | +0,15 a 0,4 ms | +0,05 a 0,15 ms |
| Radeon 780M, r16 y r32 | +0,3 a 0,6 ms | +0,8 a 1,6 ms | +0,2 a 0,5 ms |
| Losas, vallas, antorchas | exactas | bloques enteros | columnas |
| Hojas | con huecos | macizas | columnas macizas |
| Bajo aleros | bien | bien | siempre sombra |
| Entidades y celdas | dan sombra (S5) | no | no |

Costos estimados a 1280x720. Cada carril mide los suyos.

### 3. Decisiones del dueño

1. **Apagadas en el motor, prendidas por el juego.** En la 780M y otras GPU integradas, apagadas
   salvo que el jugador las pida (`on_integrated`).
2. **La sombra baja la luz del cielo un 40 %** (`strength` 0,4). Lo que da el sol queda igual que
   hoy. Se ajusta después de ver capturas.
3. **Las caras de espaldas al sol también se oscurecen**, con un paso suave cuando el sol cruza la
   pared (la rampa en `N . L`, de 0 a 1 entre 0 y 0,1, unos 6 grados).
4. **El sol va inclinado 30 grados.** Las sombras nunca caen derechas y ninguna pared queda de
   canto al sol todo el día.
5. **Hay sombras hasta 160 bloques**, y se desvanecen al final.
6. **Dan sombra los bloques, las hojas (con sus huecos), las entidades y los carteles.** El vidrio,
   el agua y las plantas chicas no. El vidrio de color no tiñe la sombra: el color ya lo pone el
   cielo de color (ADR 0064).
7. **Sin sombras de la luna.** De noche no se dibuja nada y no cuesta nada.
8. **Una fuente de cielo bajo roca, en un reino con sol, tiene sombra en High** (un 40 % más
   oscura que hoy). Un reino caverna con su propia curva no tiene sol y se ve igual que hoy.

### 4. El sol y las cascadas (CPU, carril S2)

- **Dirección.** `SunPath { tilt_degrees }` (30 por defecto), al lado de `TimeOfDay`. Con
  `theta = 2 pi * fracción del día`, la dirección hacia el sol es
  `(cos theta, sin theta * cos tilt, sin theta * sin tilt)`: sale por +X, al mediodía está a un
  seno de `cos tilt` (0,87) y se pone por -X. `sun_elevation` sigue igual para la curva del día.
- **Pasos.** El ángulo se redondea a pasos de 0,05 grados (uno cada 1/6 s con el día de 20
  minutos). Entre pasos los mapas no se mueven.
- **Cortes.** 3 cascadas sobre la distancia de vista: `[0, 20)`, `[20, 56)` y `[56, 160]`, con
  bandas de mezcla del 10 %. La última se desvanece en su último 10 %. Los cortes escalan con la
  distancia de sombras.
- **Ajuste.** Cada cascada es la esfera que contiene su tramo del frustum: no cambia de tamaño al
  girar la cámara. A lo largo de `L` va del lado lejano de la esfera hasta el tope de los chunks
  cargados más un margen.
- **Coordenadas lejanas (ADR 0014).** El centro de la caja se ajusta a texels enteros relativo a un
  ancla: el origen de render redondeado hacia abajo a un múltiplo de 1024 vóxeles, en `i64`. Dos
  escenas que difieren en un múltiplo de 1024 dan las mismas cascadas, bit a bit.
- Las matrices son relativas a la cámara, como `view_proj`.
- `RealmLight::sun: Option<bool>`: `None` sigue la curva global (tiene sol si la curva es la
  global); un reino con su propia curva no tiene sol salvo que lo diga.
- `SceneLight::sun` lleva la dirección, la fuerza por reino y el reino de la cámara. No va en
  `View`: va a `ShadowUniform`, un buffer solo de High, así el único bloque uniform de Low no
  crece.

### 5. Los casters (GPU, High, carril S3)

- `FLAG_SKY_LIT`: el bit 31 de `ChunkGpu.flags`, puesto al subir el chunk si alguna entrada de su
  paleta tiene cielo en alguna esquina. El cielo de color usa los bits 7 a 20.
- `shadow_cull.wgsl`, un dispatch propio, un hilo por slot: salta los slots sin `FLAG_LIVE`, sin
  `FLAG_SKY_LIT` o fuera de la franja del reino de la cámara. No mira la ocultación de cuevas ni el
  frustum de la cámara (un árbol detrás de la cámara da sombra delante). Prueba la caja del chunk
  contra la de cada cascada y dibuja solo las cubetas de espaldas al sol (una máscara de 6 bits que
  calcula la CPU), más la de cutout.
- Listas por cascada y página, con el mismo tope que `cull_chunks`. Una lista llena pierde casters
  (la imagen de hoy) y lo cuenta `RenderStats::shadows`.
- `shadow.wgsl`: el vertex lee el quad entero y corre `decode_quad` (formas exactas), y agranda
  cada esquina medio texel en su plano para que las grietas de las uniones en T no dejen pasar
  luz. El pipeline opaco no tiene fragment. El de cutout prueba el alfa de la textura a un mip fijo
  (las hojas dan sombra con huecos). Cruces y planos (plantas) no dan sombra.
- No se dibujan los quads translúcidos (agua, vidrio, hielo) ni el LOD lejano.
- 3 cascadas de 2048 x 2048 `Depth32Float` en una textura array: 48 MiB, unos 51 con las listas.

### 6. Los receptores (fragment, High, carriles S1 y S4)

- `common.wgsl::voxel_light` multiplica el término del cielo por un factor:
  `level_curve(l.x) * env.sky.rgb * sun_factor(i, l.x)`.
- Cada módulo que incluye `common.wgsl` agrega `sun_off.wgsl` (devuelve 1,0) o, solo en High,
  `sun_shadow.wgsl`. Multiplicar por 1,0 es exacto en IEEE 754: los módulos sin sombras dan los
  floats de hoy.
- `sun_shadow.wgsl` define `sun_factor` y el grupo 3 (el mapa, un sampler de comparación y
  `ShadowUniform`). Devuelve 1 enseguida si `k` es 0, si el cielo del fragmento es 0, si el
  fragmento es de otro reino o si está más allá de la distancia de sombras. Si no, toma la
  posición con `fragment_position`, la corre 1,5 texels por la normal, elige la cascada, hace una
  comparación PCF 2x2, aplica la rampa en `N . L` y el desvanecido, y devuelve `1 - k * (1 - v)`.
- Módulos nuevos: `VoxelPullShadowed` para High (S4), `CellShadowed` y `ModelShadowed` (S5). Mid
  sigue con `VoxelPull` y Low con `VoxelInstanced`, los dos con `sun_off`.
- High arma los pipelines con sombras la primera vez que se prenden, y usa los de hoy en los frames
  sin sombras, de noche o sin chunks con cielo al alcance.
- `RenderSettings::sun_shadows`: `enabled` (apagado), `on_integrated` (apagado), `strength` 0,4,
  `distance` 160, `cascades` 3, `resolution` 2048. Mid y Low lo ignoran.

### 7. Con todo lo demás

- **Cuevas**: nada cambia donde `S = 0`. La boca de una cueva, con `S` de 1 a 14, puede tener
  sombra.
- **Fuentes de cielo (ADR 0061)**: un reino caverna con su curva no tiene sol. Una fuente bajo roca
  en un reino con sol tiene la roca como caster (decisión 8).
- **Reinos (ADR 0039)**: ningún reino da sombra a otro. El cull deja solo los chunks entre los
  planos del reino de la cámara y `sun_factor` da 1 en los demás.
- **Vidrio de color (ADR 0064)**: el vidrio no da sombra. Bajo un techo de vitral el piso queda
  teñido y al sol. Con el cielo de color (carril C4), el mismo factor multiplica cada canal del
  cielo teñido; el nivel que se pasa es el canal más brillante.
- **Hojas**: dan sombra con huecos y la reciben como cualquier cara.
- **Agua y caras translúcidas**: no dan sombra y la reciben (la sombra de un árbol cae en un lago).
- **Celdas y modelos (ADR 0040, 0050)**: carril S5. Hasta entonces ni dan ni reciben.
- **Quads emisivos y luces dinámicas (ADR 0062, 0063)**: sin cambios.
- **LOD lejano y niebla**: el LOD queda más allá de la distancia de sombras. La niebla no cambia.

### 8. Mid y Low como hoy

En Mid y Low la GPU ve los mismos bytes que hoy: los mismos módulos (con `sun_off`), los mismos
layouts, el mismo `View`, ningún buffer, textura, paso ni atributo nuevo. Todas las pruebas de
tiers siguen comparando High, Mid y Low con sombras apagadas.

High con sombras no se puede comparar con Mid píxel por píxel (los bordes dependen de la
rasterización). `gpu_sun_shadows.rs` lo compara con High sin sombras y con un oráculo en la CPU:

- **Nunca más claro**, por píxel y canal.
- **Igual donde nada puede cambiar**: una cueva cerrada, la medianoche, fuerza 0, un reino sin sol
  y todo lo que está más allá de la distancia de sombras, byte a byte.
- **El oráculo**: un rayo exacto de vóxeles hacia el sol por píxel. La GPU tiene que decir "sombra"
  o "sol" como él, salvo a 2 texels de un borde; puede fallar como mucho el 0,5 % del resto.
- **Coordenadas lejanas**: la misma escena en desplazamientos múltiplos de 1024 da los mismos bytes.

El ayudante que corre una escena solo de High contra sus referencias lo comparten este plan y el
cielo de color (ADR 0064, decisión 1): lo hace el que llegue primero.

### 9. Carriles

| Carril | Qué | Zonas protegidas | Tamaño |
|---|---|---|---|
| S0 | este ADR | ninguna | XS |
| S1 | `sun_factor` en `voxel_light` y `sun_off.wgsl` en todos los módulos, sin cambio visible | shaders | S |
| S2 | `SunPath`, `RealmLight::sun`, `SunShadowSettings`, `SceneSun`, las cascadas | render | S a M |
| S3 | `FLAG_SKY_LIT`, `shadow_cull.wgsl`, `shadow.wgsl`, el paso de sombras, sin receptores | render y shaders | L |
| S4 | `sun_shadow.wgsl`, `VoxelPullShadowed`, el grupo 3, el cambio de pipelines | render y shaders | M a L |
| S5 | celdas y modelos dan y reciben sombra | render y shaders | M |
| S6 | línea en F3, presets de `snapshot`, caso `shadows` de `tools/perf` | ninguna | S |

S1 va antes que el C4 del cielo de color. S2 en paralelo con S1, S3 después de S2, S4 después de
S1 y S3, S5 después de S4 (y del C4, que cambia `model_shade`). Un S7 (la cascada lejana cada dos
frames, `Depth16Unorm` o 1024 en GPU integradas) solo si las medidas lo piden.

Metas del S4: en la RTX como mucho +0,25 ms en el p50 y ningún frame nuevo sobre 6,94 ms; en la
780M como mucho +0,8 ms en el p50.

### 10. Carril S1: cómo quedó

- `shaders/sun_off.wgsl` define `sun_factor(i: VsOut, sky_level: f32) -> f32` y devuelve 1,0. No
  declara nada más: ningún global, struct, binding ni atributo.
- `voxel_light` multiplica el término del cielo por `sun_factor(i, l.x)`. Un comentario ahí dice
  dónde va el factor del camino teñido del C4.
- Los 8 módulos que incluyen `common.wgsl` (`VoxelPull`, `VoxelInstanced`, `Cull`, `Lod`,
  `LodPull`, `LodCull`, `Model` y `Cell`) llevan `sun_off.wgsl` justo después
  (`SUN_OFF_WGSL`). Todos lo necesitan, aunque no sombreen: WGSL valida cada función del módulo.
- `model_shade` (S5) y `lod_common.wgsl` no cambian: tienen su propia fórmula de luz.
- Prueba nueva en `tests/naga.rs`: cada módulo con `common.wgsl` tiene `sun_off.wgsl`;
  `sun_factor` es un solo `return` del literal 1,0 y no lee ningún global; `voxel_light` lo llama;
  ningún módulo de vóxeles, celdas o modelos usa el grupo 3. Las pruebas de siempre (`View` de
  1408 + 8192 B, un bloque uniform en Low, dos atributos de instancia) siguen pasando.
- Las pruebas de GPU pasan sin cambios en los tres tiers.

Para el S4: `sun_shadow.wgsl` tiene que definir la misma firma. El nivel llega sin la curva (0 a
15, interpolado), así que `sky_level == 0.0` es la salida rápida de las cuevas. `VsOut` ya trae
lo que pide la sección 6: `clip` para `fragment_position`, `view_dist`, `face` y el reino en
`ao`.

### 11. Carril S2: cómo quedó

Solo CPU: ningún shader, buffer ni pase cambia, y los tres tiers dibujan lo mismo que antes.

- `boxel_world::SunPath` (inclinación 30 grados), en `DaylightPlugin`. `SunPath::step` da el paso
  del sol desde los ticks, exacto: 7200 pasos al día, uno cada 10 2/3 ticks.
- `RealmLight::sun` y `RealmLight::has_sun`.
- `RenderSettings::sun_shadows` (`SunShadowSettings`): apagadas, 0,4, 160 bloques, 3 cascadas de
  2048. Un cambio inválido se registra y quedan las últimas válidas. `applies(tier, device_type)`
  dice si un renderer las dibuja: solo High, y en GPU integradas solo con `on_integrated`.
- `SceneLight::sun` (`SceneSun`): la dirección, `k` por reino y el reino de la cámara. El extract
  lo llena mientras las sombras están prendidas. `k` ya trae el desvanecido junto al horizonte.
- `boxel_render::shadow::shadow_cascades` da las cascadas de un frame, o nada si `k` del reino de
  la cámara es 0. Cada `Cascade` trae la matriz y los 6 planos relativos a la cámara, el tramo de
  distancias que sirve, el tamaño del texel y una clave entera (centro en texels y profundidad en
  pasos de 8 bloques) que fija la caja en el mundo. Con 3 cascadas los cortes son los del plan
  (0, 20, 56 y 160; cada una empieza un 10 % antes del final de la anterior).
- La caja sube hacia el sol hasta `ShadowView::casters_top` (el tope de los chunks cargados, que
  pone el S3), con un tope de 512 bloques más allá de la esfera.
- `bucket_mask` da las caras que dan sombra: las que miran lejos del sol. Las de canto no.
- Pruebas (proptest): la cascada contiene su tramo del frustum y los casters de encima con
  cualquier orientación, fov y aspecto; al moverse, un punto del mundo queda en el mismo lugar
  dentro de su texel; girar no cambia el tamaño; a 2^31, 2^35, 2^40 y el límite del mundo las
  cascadas salen iguales bit a bit.

Para el S3: las cascadas cambian en cada frame en que cambia el origen de render (son relativas a
la cámara), pero la caja en el mundo solo cambia con su clave, el ancla o el paso del sol. Las
estructuras para la GPU (`ShadowUniform`) las define el S3 junto con su WGSL.

### 12. Carril S3: cómo quedó

Solo High y solo con las sombras prendidas. Nadie lee los mapas todavía: la imagen es la misma
con sombras y sin ellas, en los tres tiers.

- `FLAG_SKY_LIT`, el bit 31 de `ChunkGpu.flags`, se pone al subir un chunk si alguna esquina de
  su paleta tiene cielo (una entrada teñida del cielo de color cuenta siempre). La tabla de chunks
  guarda el tope de los chunks con cielo por altura: es el `casters_top` de las cascadas, cortado
  al plano de arriba del reino de la cámara. Sin chunks con cielo en el reino no se dibuja nada.
- `shadow_cull.wgsl` (`cull_shadow_casters`): un hilo por slot, como `cull.wgsl`. Salta los
  slots sin `FLAG_LIVE`, sin `FLAG_SKY_LIT` o fuera de la franja del reino de la cámara. Por
  cada cascada cuya caja toca el chunk, escribe las cubetas de espaldas al sol (`bucket_mask`)
  y la de cutout. Listas por cascada y página: `3C` opacas y `C` de cutout, con
  `C = min(chunks de una página, 2048)`. Una lista llena pierde la cubeta y la cuenta.
- `shadow.wgsl`: `vs_shadow` (opacas, sin fragment) y `vs_shadow_cutout` + `fs_shadow_cutout`
  (alfa a un mip fijo según el texel de la cascada). Cada esquina crece medio texel en su plano.
  En cutout se colapsan las caras que no dan la espalda al sol, y las cruces y planos.
  Sesgo: constante 1, pendiente 1,5. `cull_mode: None`, comparación `LessEqual`.
- `shadow/pass.rs` (`ShadowPass`): una textura `Depth32Float` array (una capa por cascada,
  `resolution` al cuadrado), el uniform, las listas y el estado del cull. Un pase de profundidad
  por cascada después del cull de High y antes del pase principal, limpiado a 1. Sin sombras,
  de noche o sin chunks con cielo no se graba nada.
- `VoxelRenderer::set_sun_shadows` lo llama el `prepare` del plugin en cada frame, como
  `set_memory_limits`. Mira `SunShadowSettings::applies` contra el tipo de adaptador: en Mid,
  Low, con las sombras apagadas o en una GPU integrada sin `on_integrated` no existe nada.
- `ViewParams::lens` (`CameraLens`): la dirección, el fov, el aspecto y la distancia de la
  cámara, que las cascadas necesitan. Lo llena `ViewParams::from_camera`.
- `RenderStats::shadows` (`ShadowStats`, también en `RenderInfo`): cascadas dibujadas,
  multi-draws, cubetas perdidas (leídas uno o dos frames tarde) y bytes.
- `stress --sun-shadows`: prende las sombras, también en GPU integradas, y al final dice qué
  dibujó el último frame.
- El espejo de la GPU es `boxel_shaders::gpu::shadow::ShadowCascadeGpu` (256 B por cascada,
  `shadow_common.wgsl`). El uniform tiene 4 entradas; la entrada 0 lleva lo que comparte el frame.

Pruebas: `tests/naga.rs` (los módulos nuevos son solo de High, los espejos y constantes, las
escrituras del cull quedan en sus listas); `gpu_sun_shadows.rs` en la RTX, la 780M y llvmpipe:
cada texel de cada cascada guarda la profundidad de la primera cara de espaldas que encuentra un
rayo exacto en la CPU (a las 15:00 y a las 17:00; fallan unos 50 de 250.000 texels, todos junto a
bordes); vidrio, agua y plantas no dejan nada; las hojas dan sombra con algunos huecos; un chunk
oscuro y un chunk de otro reino no dibujan nada; la imagen principal es igual byte a byte con
sombras y sin ellas; de noche no se dibuja nada; a 2^31, 2^40 y junto al límite del mundo los
mapas salen iguales bit a bit; en Mid y Low no hay pase ni recursos y la imagen no cambia.

Para el S4: los mapas están en `ShadowPass::maps` (`Depth32Float`, `D2Array`, con
`TEXTURE_BINDING`), cada capa limpiada a 1 y con la profundidad del caster más cercano al sol.
Un receptor está al sol si su profundidad es como mucho la del mapa (`LessEqual`). Para pasar a
UV: `x = (ndc.x + 1) / 2`, `y = (1 - ndc.y) / 2`. El uniform (`ShadowCascade`) ya trae la
matriz, el texel, el tramo de distancias, la dirección del sol, la distancia de sombras, el
reino de la cámara y 32 bytes libres al final de cada entrada. Falta `k` por reino, que hoy no
está en el uniform.

## Medidas

Carril S1, `just perf-smoke surface` contra main (778e7bb), una corrida de 10 s por lado. No
cambia ninguna malla ni código de CPU, así que no hay números de `headless_world` ni de criterion
que mirar.

| Caso | fps | p99 ms | Frames sobre 6,94 ms | Veredicto |
|---|---:|---:|---:|---|
| rtx/surface/r16/uncapped | 2017 -> 2036 (+1,0 %) | 1,24 -> 1,08 | 0 -> 0 | ok |

p50 0,45 ms en los dos lados: el factor 1,0 no se nota.

Carril S2: no cambia nada en la GPU, así que no hay `perf-smoke`. Una prueba unitaria mide las
cascadas: 4 cascadas tardan unos 0,3 µs por frame.

Carril S3, `just perf-smoke` contra main (7817f35), con `--sun-shadows` solo en el lado nuevo:
mide el cull y los tres pases de profundidad, sin receptores. No cambia el mundo ni el mallador,
así que `headless_world` da los mismos quads y chunks.

| Caso | fps | p50 ms | p99 ms | Frames sobre 6,94 ms | Veredicto |
|---|---:|---:|---:|---:|---|
| rtx/surface/r16/uncapped | 1878 -> 1612 (-14,2 %) | 0,48 -> 0,56 | 1,57 -> 1,74 | 0 -> 0 | ok |
| rtx/surface/r32/uncapped | 703 -> 640 (-8,9 %) | 1,28 -> 1,32 | 3,58 -> 4,07 | 0 -> 0 | ok |
| 780m/surface/r16/uncapped | 590 -> 344 (-41,8 %) | 1,49 -> 2,73 | 3,93 -> 6,96 | 1 -> 36 | peor |

En la RTX los pases cuestan unos 0,08 ms de p50 en r16 y entre 0,04 y 0,14 ms en r32 (p50 y
media): lo que estimaba el plan, igual en los dos radios. En la 780M cuestan unos 1,25 ms, el
doble de lo estimado y más que la meta del S4 (+0,8 ms con receptores). Allí las sombras vienen
apagadas (decisión 1); si se quieren prender, hacen falta las palancas del S7. Cada frame dibujó 3
cascadas, 24 multi-draws en r16 y 42 en r32, sin cubetas perdidas; los recursos ocupan 53 MiB.

Con las sombras apagadas: `rtx/surface/r16/uncapped` 2002 -> 1953 fps (-2,5 %), p50 0,45 ->
0,46 ms, p99 1,10 -> 1,20 ms (una primera corrida dio -7,4 %; repetida, dentro del ruido).

## El juego

- Prende `sun_shadows.enabled`. En GPU integradas solo si el jugador lo pide.
- Los reinos con su propia curva (cavernas, el reino tipo Nether) no tienen sol salvo que lo digan.

## Consecuencias

- De día, en High, los bloques, las hojas y (desde el S5) las entidades dan sombras nítidas que se
  mueven con el sol. De noche todo queda como hoy.
- Mid y Low no cambian. High con las sombras apagadas tampoco.
- High con sombras deja de ser comparable píxel a píxel con Mid: tiene sus propias referencias.
- 51 MiB más de VRAM en High con sombras. El costo no crece con el radio.
- Una fuente de cielo bajo roca en un reino con sol se ve más oscura en High que en Mid y Low.
- Las sombras terminan a 160 bloques: en vistas largas y planas se ve una banda suave.

## Alternativas descartadas

- **B, marchar un volumen de ocupación.** Exacto para bloques, pero cuesta por píxel justo en la
  780M, pide un camino de datos nuevo del mundo al render, y las losas, vallas y hojas dan sombra
  de bloque entero. Celdas y modelos no dan sombra.
- **C, mapa de alturas de los techos.** Barato, pero bajo cualquier alero o puente siempre hay
  sombra, y los árboles dan columnas macizas.
- **Hornear la visibilidad del sol en las mallas.** El sol se mueve: cada chunk se remallaría cada
  pocos minutos de juego.
- **Ray queries por hardware.** Cientos de MiB de estructuras, una reconstrucción por remallado, y
  en `wgpu` siguen experimentales.
- **Sombras en espacio de pantalla.** Pierden todo caster fuera de la pantalla.
- **A con C lejos** (sombras de montañas al atardecer): queda para después.
