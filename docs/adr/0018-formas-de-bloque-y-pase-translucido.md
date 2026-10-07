# ADR 0018: formas de bloque y pase translúcido

- Estado: aceptado (primera parte de la issue #38, y la issue #16). Las variantes de tronco y de
  losa pasaron a ser estados de bloque en el ADR 0019, que cambia los ids de esta parte. Las
  losas se dibujan como cajas desde el ADR 0020.
- Fecha: 2026-10-07

## Contexto

Hasta acá todo bloque era un cubo. El agua ya caía en el bucket translúcido (ADR 0012), pero ese
bucket no se dibujaba. Para un clon de Minecraft faltan losas, plantas en cruz, troncos acostados,
vidrio y agua visibles. `BlockDef` tenía `solid: bool` y la física solo conocía cubos llenos
(ADR 0016 dejó `BlockDef::collision` para cuando llegaran las formas). La idea era romper
`BlockDef` una sola vez para render y colisión.

## Decisión

### 1. Datos de bloque

- `BlockDef` cambia `solid` por `collision: BlockCollision` (`None`, `Full` o una lista de
  `CollisionBox` en dieciseisavos, exactos en binario) y suma `model: BlockModel`:
  `Cube { axis }` (el eje gira la textura, para troncos), `Slab(Bottom | Top)` y `Cross`.
- `RenderShape` pasa a ser la capa de dibujo. Solo un cubo `Opaque` tapa caras vecinas
  (`StateFlags::OCCLUDES`, `BlockRegistry::occludes`) y exige opacidad de luz 15. `SOLID` sale
  de `collision`.
- Contenido nuevo (estados 17 a 23): vidrio (cubo translúcido, sólido, opacidad 0), losa de
  piedra abajo y arriba (opacas, opacidad 1), pasto corto y amapola (cruces cutout, sin colisión,
  opacidad 0), tronco en X y en Z. El agua tiene alfa.
- Las losas no tapan la luz del todo. Opacidad 1, como las hojas: con un valor de 2 a 14 la
  inundación de luz entra en su camino lento para todos los chunks. Una losa de techo deja pasar
  luz como una copa de hojas.

### 2. Malla

- Clase nueva `VoxelClass::Special` para los bloques que no son cubos. El greedy los ve como
  aire (sus vecinos dibujan la cara contra ellos). Un paso por bloque emite sus quads con
  `w = h = 1`, después del greedy, al final de cada bucket.
- `PackedQuad` sigue en 8 bytes: los 3 bits libres de flags (29 a 31) son un código de forma.
  0 cubo, 1 cubo con textura girada (troncos en X o Z, sigue en el greedy), 2 mitad de abajo,
  3 mitad de arriba (losas), 4 cruz. El shader arma la geometría con el código.
- Una cara de losa en el borde de la celda se oculta si enfrente hay un cubo opaco o una cara
  vecina que la cubre (dos losas de abajo lado a lado). La cara de adentro (el techo de una losa
  de abajo) siempre se dibuja. Un cubo nunca se oculta contra una losa: la cara que queda debajo
  se dibuja y la tapa la profundidad.
- Sombreado exacto: una cara de losa usa la misma regla de luz y AO que la cara del cubo en su
  lugar, y el shader mueve `t` con la altura real. La media cara se ve igual que esa parte de la
  cara del cubo. `reference_face_shading_at` es el oráculo. Una cruz toma la luz de su propia
  celda, sin AO y sin sombreado de cara.
- Una cruz son dos quads en las diagonales, en el bucket cutout (se ven de los dos lados).

### 3. Pase translúcido

- Tercer pipeline por tier, `fs_translucent`: mezcla alfa, test de profundidad sin escritura,
  sin culling de caras (la superficie del agua se ve desde abajo). Va después del opaco y del
  cutout.
- Orden: una lista en CPU, igual para los tres tiers, con los chunks visibles que tienen quads
  translúcidos, de atrás hacia adelante por distancia al centro del chunk (empate por slot).
  High la dibuja con un `draw_indexed` por chunk desde un buffer chico de `DrawRef`; Mid y Low
  como sus otros buckets. Así la mezcla da los mismos píxeles en los tres tiers.
- Dentro de un chunk los quads translúcidos también van de atrás hacia adelante
  (`boxel_render::sort`). Un trabajo reordena el bucket translúcido del chunk para la cámara y el
  renderer sube los quads permutados sobre el mismo rango del arena (en Low, también su luz en el
  buffer paralelo). Los shaders y los draws no cambian, así que los tres tiers dibujan lo mismo.
- El orden no es la distancia al centro de cada quad, como en Minecraft. Con quads greedy no
  alcanza: el centro de una superficie de agua de 32x32 queda lejos aunque su parte cercana tape
  un vidrio hundido junto a la cámara. Es un orden topológico de "puede tapar": si un quad queda
  entero de un lado del plano de otro, el lado de la cámara decide cuál va primero. Dos caras en
  el mismo plano (agua contra vidrio) van primero la que mira hacia afuera de la cámara. Lo que
  ningún plano separa, y los ciclos (raros), se resuelven por distancia al centro. Es exacto para
  quads que no se cruzan, y un test lo compara con rayos.
- Cuándo: el orden depende solo de qué lado de cada plano está la cámara. Se reordena cuando la
  cámara cruza uno de los planos del chunk. Los chunks a más de 64 vóxeles esperan además a que
  la cámara se mueva un octavo de su distancia (un quad recién cruzado se ve de canto y de lejos
  no tapa casi nada). Un chunk de más de 512 quads translúcidos usa solo la distancia al centro
  (el par a par es cuadrático) y se reordena al cambiar de bloque, con la misma holgura.
- Costo: los chunks sin quads translúcidos no tienen estado. Los trabajos corren en el pool de
  cómputo asíncrono (los `JobExecutor` del mundo), los más cercanos y visibles primero, con un
  tope de 64 Ki quads en vuelo. Las subidas van en el staging del frame, con un tope de 32 Ki
  quads por frame, y se pierden y se reenvían con el resto en un resync. Un chunk nuevo o
  remallado se ve un frame en el orden del mesher, hasta que llega su orden.
- `TranslucentSort` elige `Background` (por defecto), `Inline` (en el mismo frame, para tests y
  capturas) u `Off`.
- Las caras entre dos bloques translúcidos iguales se omiten (no hay paredes de agua adentro del
  agua). Entre agua y vidrio se dibujan las dos.

### 4. Física

- `CollisionTable` guarda las cajas de cada estado y un contorno para apuntar, que sale del
  modelo (cubo, media celda, una caja chica alrededor de la planta).
- Barrido: un bloque de cajas frena en la cara más cercana de sus cajas que pisan la huella.
  Con cajas en la tabla se mira también la capa donde está la cara de adelante. Para cubos
  llenos el resultado no cambia.
- La salida de un bloque solo para en planos enteros: una caja dentro de una losa sube al plano
  siguiente, no al techo de la losa.
- Raycast: `Collision` usa las cajas de colisión y `Outline` el contorno. El punto de impacto
  queda sobre el plano de la cara de la caja.
- `PhysicsQuery::state_is_free(pos, estado)`: el chequeo para poner un bloque con su forma.
  Una losa solo mira su mitad, una planta siempre entra.

### 5. Mundo y ejemplos

- El forest y el overworld ponen pasto corto (12 %) y amapolas (1.2 %) sobre el pasto, con un
  hash por columna con su propia sal. Sin agua en el mundo todavía.
- `voxel_world`: la rueda del mouse recorre la barra (vidrio, losa, pasto, amapola y agua
  además de las teclas 1 a 0). La losa va arriba si el clic toca una cara de abajo o la mitad de
  arriba de un costado. El tronco se acuesta en el eje de la cara clickeada.

### 6. Superficie del agua

El agua usa `BlockModel::Fluid`: para la malla y el culling es un cubo, pero su celda de
superficie tiene el techo a 14/16 de bloque, como en Minecraft (que usa 8/9). Es superficie el
agua que no tiene encima ni agua ni un bloque opaco. Bajo un bloque opaco el agua queda a altura
completa, así no se ve un hueco debajo de un muelle.

- El gather marca la superficie con el bit alto del id de material (`FLUID_SURFACE_BIT`). El
  culling lo ignora (`PaddedChunk::cull_mask`), así no aparecen caras entre la superficie y el
  agua de abajo. La fusión greedy sí lo ve, así la capa de superficie forma sus propios quads.
- Esos quads llevan el código de forma `SHAPE_FLUID_SURFACE`: la cara de arriba baja 2/16 y los
  costados terminan ahí. La cara de abajo es la del cubo. Siguen siendo quads greedy, así un lago
  no cuesta más quads que antes (solo se separa la capa de arriba en los costados).
- Solo se marca si todos los ids de material caben debajo del bit (hasta 32 768). Si no, el agua
  conserva el techo completo.
- Queda una ranura de 2/16 donde un agua de altura completa (bajo un bloque) toca de costado un
  agua de superficie. Es raro y se va con los líquidos que fluyen, que tendrán altura por esquina.

## Consecuencias

- Costo medido en el RTX 5050 (stress fly r16 a 32 vóxeles/s, sin tope, 2 corridas): main 4614
  y 4565 fps, la rama 3594 y 3583 fps (p50 0.19 a 0.25 ms, p99 0.64 a 0.70 ms). Los quads del
  forest suben de 891 325 a 1 136 977 (+27.6 %), casi todo por las plantas. Sin plantas
  (`--world terrain`) la diferencia es 7408 contra 7314 fps (ruido).
- Una losa cuesta hasta 6 quads sin fusionar; las plantas 2 por bloque.
- La celda de una losa de techo recibe luz como si fuera una hoja.

## Fuera de esta parte

Líquidos que fluyen. Escaleras, vallas y antorchas están en el ADR 0020, que también lleva las
losas a caras de caja (los códigos de forma 2 y 3 de este ADR quedan libres). Los estados de
bloque ya están hechos
(ADR 0019): `boxel:log[axis=x|y|z]` reemplaza a `boxel:log_x` y `boxel:log_z`, y
`boxel:stone_slab[type=bottom|top|double]` a `boxel:stone_slab_top`, con la losa doble.

## Alternativas descartadas

- Meshear las losas en el greedy: cada media cara necesita su propia clave y su propio plano, y
  el fork de bgm compara caras de cubo enteras.
- Una extensión de `PackedQuad` a 12 o 16 bytes: todos los tiers pagan más memoria y ancho de
  banda por formas que son pocas.
- Ordenar los quads translúcidos dentro del chunk en cada frame: costo de CPU y de subida por
  frame. Se reordena solo cuando la cámara cruza un plano (ver la sección 3).
- Ordenar por distancia al centro de cada quad, como Minecraft: falla con quads greedy grandes.
- Un buffer de índices por chunk con el orden: Low no tiene storage buffers y los quads ya son
  atributos de instancia; reescribir el rango del arena sirve igual en los tres tiers.
