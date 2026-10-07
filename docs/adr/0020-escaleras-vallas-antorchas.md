# ADR 0020: escaleras, vallas y antorchas

- Estado: aceptado (tercera parte de la issue #38)
- Fecha: 2026-10-07
- Revisa: ADR 0018 (las losas pasan a ser modelos de cajas y los códigos de forma 2 y 3 quedan
  libres)

## Contexto

Con el ADR 0019 ya hay estados de bloque, pero el mesher solo sabe dibujar cubos, medias celdas
(losas) y cruces. Una escalera, una valla o una antorcha son uniones de cajas en dieciseisavos
de bloque, como los modelos de Minecraft. Un quad especial tiene `w = h = 1` y 3 bits de código
de forma: no hay lugar para una caja arbitraria (25 bits entre rectángulo y plano).

Además, la forma de una escalera y los lados de una valla dependen de los vecinos. Hacía falta un
mecanismo del motor que recalcule el estado de un bloque cuando cambia un vecino.

## Decisión

### 1. Modelos de cajas

- `BlockModel` suma `Stairs { facing, half, shape }`, `Fence { sides }`, `Torch` y
  `WallTorch { facing }`. `BlockModel::boxes` da sus cajas (`ModelBox`): un `CollisionBox` en
  dieciseisavos y un desplazamiento de textura por eje. Las losas también son una caja.
- Las formas están en `boxel_voxel::shapes`, escritas para una orientación y giradas de a un
  cuarto de vuelta alrededor del eje vertical de la celda, como Minecraft gira sus modelos:
  - Escalera (para `east`, abajo): la losa `[0,0,0]-[16,8,16]` y el escalón `[8,8,0]-[16,16,16]`.
    `outer_*` deja un cuarto del escalón; `inner_*` le agrega un cuarto del lado bajo. Las de
    arriba son las de abajo invertidas en `y`. Izquierda y derecha se miran hacia `facing`.
  - Valla: poste `[6,0,6]-[10,16,10]` y, hacia cada lado conectado, dos barras de 2x3 del poste
    al borde (`fence_side` de Minecraft).
  - Antorcha: palo `[7,0,7]-[9,10,9]`. La de pared en Minecraft está inclinada 22,5 grados; acá
    es el mismo palo derecho, pegado a la pared y subido 3 dieciseisavos, con un desplazamiento
    de textura que le hace mostrar los mismos texeles que la antorcha de pie.
- Colisión y contorno salen del modelo: la escalera choca y se apunta con sus cajas; la valla con
  el poste y brazos de 4 de ancho (un bloque de alto, ver sección 6); las antorchas no chocan y
  se apuntan con una caja chica.
- El registro valida las cajas y cuenta las distintas: como mucho `MAX_MODEL_BOXES` = 1024
  (`RegistryError::Full("model boxes")`). El contenido de serie usa menos de 64.

### 2. Contenido

| Bloque | Estados | Notas |
| --- | --- | --- |
| `boxel:stone_stairs`, `boxel:oak_stairs` | 25 a 104 y 105 a 184 | `facing`, `half`, `shape`, `waterlogged` (80 cada una) |
| `boxel:oak_fence` | 185 a 200 | `east`, `north`, `south`, `west` booleanos |
| `boxel:torch` | 201 | cutout, emisión (14, 13, 10) |
| `boxel:wall_torch` | 202 a 205 | `facing` (hacia afuera de la pared) |

- Las escaleras usan las texturas de piedra y de tablones; la valla, la de tablones; las
  antorchas, una capa nueva `boxel:torch` con el dibujo de Minecraft (palo en las columnas 7 y 8
  desde la fila 8, llama en las filas 6 y 7). El resource pack la lee de `block/torch`.
- `waterlogged` es la propiedad de Minecraft. Todavía no hace nada: llega con los fluidos.
- Los ids anteriores no cambian. Los ids no se guardan en disco.

### 3. Malla: caras de caja

- Código de forma nuevo `SHAPE_BOX` = 6. Los 10 bits de `w - 1` y `h - 1` de un quad especial
  guardan el índice de su caja en una tabla global de cajas. `MaterialTable` la arma al recorrer
  los materiales (orden de primer uso, sin repetidos) y la da con `model_boxes`. Las losas se
  dibujan así también, y los códigos 2 y 3 del ADR 0018 quedan libres. El 7 sigue libre.
- Una caja dibuja sus 6 caras salvo las que tapan del todo otras cajas del mismo modelo (el piso
  del escalón sobre la losa, la punta de una barra contra el poste). Como Minecraft, el techo de
  la losa bajo el escalón se dibuja entero y lo tapa la profundidad.
- Una cara en el borde de la celda se oculta si enfrente hay un cubo opaco, o un modelo de cajas
  opaco (o del mismo material) cuyas caras en ese borde la cubren. Se compara con una máscara de
  16x16 dieciseisavos por cara. Las caras de adentro de la celda siempre se dibujan. Un cubo
  nunca se oculta contra un modelo de cajas, como con las losas.
- Sombreado: cada cara se sombrea como la parte de la cara del cubo que cubre (luz y AO de la
  capa de enfrente, `(s, t)` = su lugar en esa cara). Si la celda de enfrente es un cubo opaco,
  usa la capa de la propia celda: así el poste de una valla contra una pared o el escalón bajo
  un bloque no salen negros. Una cara en el borde con un cubo opaco enfrente ya está oculta, así
  que esto solo cambia caras de adentro. `reference_box_face_shading_at` es el oráculo.
- Textura: la parte de la textura de la cara del cubo que queda bajo la caja, movida por su
  desplazamiento (el `uvlock` de Minecraft). Todos los valores son dieciseisavos: exactos.
- Costo: los chunks sin estos bloques no cambian. La tabla de un material es un índice más
  (`u16`) y el paso por bloque solo corre sobre los vóxeles `Special`.

### 4. GPU

- La tabla va en el uniform `View`, después de su parte por frame: `boxes: array<vec4<u32>,
  512>` (dos cajas por `vec4`, 8 KiB). Palabra 0: mínimo y máximo, 5 bits por coordenada;
  palabra 1: el desplazamiento de textura, 6 bits por eje. `VoxelRenderer::set_model_boxes` la
  escribe una vez (el plugin lo hace al crear el renderer); el per-frame sigue escribiendo solo
  sus 352 bytes. Low mantiene un solo uniform block (ADR 0011) y su límite de 16 KiB alcanza.
- `decode_quad` arma la cara con la caja: plano, rango en `u` y `v`, textura y `(s, t)`, todo en
  dieciseisavos exactos, igual en los tres tiers. El crecimiento de bordes del ADR 0015 sigue
  igual.
- El orden de los quads translúcidos (`sort.rs`) lee la misma tabla para el plano y el rectángulo
  de cada cara de caja. Un índice fuera de la tabla cuenta como cara de cubo.

### 5. Estados que siguen a sus vecinos

- `boxel_voxel::rules`: funciones puras `(registro, estado, 6 vecinos) -> estado`.
  - Escaleras: la regla de Minecraft (`StairBlock::getStairsShape`). Escaleras de la misma mitad
    del lado alto y cruzadas forman una esquina exterior; si no, del lado bajo, una interior. No
    se toma la esquina si el lado hacia el que abre tiene escaleras iguales (siguen la fila).
    Cuentan escaleras de cualquier material.
  - Vallas: se conectan a otras vallas y a caras firmes (la colisión cubre la cara entera),
    salvo bloques cutout (las hojas, la excepción de Minecraft). Simplificado: sin puertas de
    valla.
  - Antorchas: la de pie cae (pasa a aire) sin un bloque abajo que sostenga su centro (un poste
    de valla sirve); la de pared, sin una cara firme detrás.
- `boxel_world::NeighborRules` (recurso, `src/neighbor_rules.rs`) guarda la regla de cada
  bloque. `WorldPlugin` pone las de serie según el modelo. En `WorldSystems::Edits`, antes de
  aplicar el lote, se recorren las posiciones editadas y sus 6 vecinos: si su bloque tiene
  regla, se aplica contra el mapa con las ediciones del lote, y un cambio entra al mismo lote
  como una edición más. Así poner una escalera le da su forma y gira las de al lado en el mismo
  frame, con un solo remallado y un `BlockChanged` por bloque.
- Una sola vuelta: lo que cambió una regla no se vuelve a mirar. Las reglas de serie no lo
  necesitan, porque ninguna lee lo que otra cambia (si un vecino es escalera y su `facing` y
  `half`, si es valla, su colisión). Una regla nueva debe cumplir lo mismo.
- Si un vecino está en un chunk que no se puede leer, el bloque queda como está: un borde de
  chunk no tira antorchas ni corta vallas.
- Va separado de los ticks programados (los fluidos): no guarda estado entre frames y solo ve un
  lote de ediciones.
- El generador no pone estos bloques.

### 6. Luz y física

- Las escaleras dejan pasar la luz como las losas (opacidad 1: frenan el cielo directo y la luz
  pierde un nivel por vóxel, como en el aire). Vallas y antorchas tienen opacidad 0. La antorcha
  emite (14, 13, 10), un blanco cálido de nivel 14, y se dibuja con brillo pleno como las
  lámparas (`FLAG_EMISSIVE`).
- La escalera choca con sus cajas y el jugador la sube con el step-up del controlador (medio
  bloque por vez). Su espalda es una pared de un bloque.
- Valla: en Minecraft choca a 1,5 bloques. Acá queda en 1 bloque. El barrido supone que las
  cajas de un bloque están dentro de su celda: la huella de una caja que se mueve, el orden en
  que las capas deciden el plano más cercano, la salida de un bloque y el raycast por celdas.
  Una caja que sale de la celda pide cambios en todos esos lugares del núcleo de la física, que
  otro trabajo está tocando a la vez. Queda anotado para una issue.

### 7. Ejemplos

- `voxel_world`: la rueda del mouse suma escaleras de piedra y de roble, la valla y la
  antorcha. La escalera mira hacia donde mira el jugador (su lado alto lejos de él) y va arriba o
  abajo según el clic, como las losas. La antorcha va de pie en una cara de arriba, de pared en
  una cara de costado (mirando hacia afuera) y no va debajo de un bloque.
- `snapshot` sube la tabla de cajas y cuenta como aplicada una edición cuyo bloque quedó aunque
  su estado cambió por una regla. Por ejemplo, en el mundo plano:
  `--world flat --camera 5,68.5,12.5,0,-30 --place 'stone_stairs[facing=east]@0,64,3'
  --place 'stone_stairs[facing=north]@1,64,3' --fill 'oak_fence@1,64,6:4,64,6'
  --place 'torch@2,65,6' --shot 21:00=noche.png` (una esquina exterior, una valla con una
  antorcha encima, de noche).

## Consecuencias

- Los chunks sin estos bloques cuestan lo mismo. `cargo bench -p boxel_mesh` en el Ryzen 7 250,
  7 corridas alternadas por versión con otros agentes compilando a ratos, mejor tiempo de cada
  una (main contra la rama): `mesh/terrain` 25,7 y 25,6 µs, `mesh/surface` 89,0 y 88,8 µs,
  `gather_mesh/terrain_packed` 98,1 y 98,1 µs, `gather_mesh/cave_lit` 115,3 y 115,5 µs,
  `gather_mesh/surface` 159,4 y 163,3 µs (+2,4 %, el único fuera de ±1 %; tiene plantas, que
  pasan por el camino por bloque). Las medianas variaron hasta 30 % por la carga.
- Escena nueva `mesh/shapes`: el terreno con un bloque de cajas en un cuarto de las celdas de la
  superficie (2080 caras de caja de 3263 quads): 50,8 µs contra 25,7 µs del terreno solo, unos
  12 ns por cara de caja. `gather_mesh/shapes`: 127 µs.
- En el aire, una valla conectada a los cuatro lados son 46 quads y una escalera de 11 a 15
  (sin fusionar, como las losas). Las caras contra vecinos se ocultan.
- El uniform `View` pasa de 352 a 8544 bytes, pero por frame se siguen escribiendo 352.
- Los códigos de forma 2 y 3 quedan libres para lo que venga (los fluidos que fluyen, por
  ejemplo).

## Fuera de esta parte

- Valla de 1,5 bloques de alto (sección 6).
- Antorcha de pared inclinada como en Minecraft.
- Puertas de valla, paneles, muros, escaleras con agua (`waterlogged`).
- Que un cubo oculte su cara contra la espalda llena de una escalera: hoy se dibuja y la tapa la
  profundidad, como con las losas.

## Alternativas descartadas

- Una tabla de cajas por chunk, como la paleta de luz: en Low iría en un tercer vertex buffer
  por instancia (otros 64 MiB fijos de VRAM) o en el mismo de la luz, que ya está lleno.
- Agrandar `PackedQuad`: todos los tiers pagan más memoria por formas que son pocas.
- La tabla como constante del shader: depende del registro, así que habría que componer y
  compilar los pipelines con cada contenido.
- La tabla en una capa de la textura: la textura es sRGB y Low solo tiene un par de textura y
  sampler.
- Dejar las losas en sus códigos: dos caminos para lo mismo, y las losas no se ocultarían contra
  las escaleras.
