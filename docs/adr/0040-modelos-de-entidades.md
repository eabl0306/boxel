# ADR 0040: modelos de entidades

- Estado: aceptado (el dueño pidió el jugador de boxelcraft, con el trabajo de render que hace
  falta en el motor)
- Fecha: 2026-10-26

## Contexto

El juego necesita dibujar a su jugador, y después mobs (issue #40). El motor no dibujaba
entidades: solo chunks, el terreno lejano, la niebla, el contorno y la interfaz. Hacen falta
pocas instancias por ahora, en los tres tiers, con los mismos píxeles en todos.

## Decisión

### 1. Un modelo es un árbol de cajas con su textura

- `boxel_render::model`: `Model` (partes con padre, pose de reposo y cajas), `Cuboid` (origen,
  tamaño, `inflate`, `mirror`, material), `Pose` (por parte: desplazamiento, rotación y si se ve)
  y `ModelTexture` (RGBA8 sRGB con un id único en el proceso).
- El espacio del modelo es el de los modelos de entidades de Minecraft Java: píxeles, +Y hacia
  abajo, el modelo mira a -Z y la rotación de una parte es `Rz * Ry * Rx` en su pivote. Así un
  modelo y sus animaciones se copian de Minecraft (o de Blockbench) sin cambiar signos. El motor
  da vuelta el modelo (`scale(-1, -1, 1)`, media vuelta en z) al ponerlo en el mundo.
- Cada caja toma su textura con el box-UV de Minecraft: las caras salen de `uv` y del tamaño, cada
  una vista desde afuera, como `ModelPart.Cube`. `inflate` agranda la caja sin cambiar su
  textura (capas de afuera) y `mirror` la refleja en x.
- Material por caja: opaco (sin alfa, caras de atrás descartadas) o cutout (test de alfa, las dos
  caras: el interior de un sombrero se ve por sus huecos).

### 2. La CPU posa las cajas y los tres tiers dibujan lo mismo

- Cada frame la CPU pasa las cajas de cada instancia a vértices relativos al origen de render
  (`ModelScene`): 4 por cara, 32 bytes cada uno (posición, texel, el rectángulo de la cara en la
  textura y un `u32` con la luz y el sombreado de la cara). Un solo buffer de vértices y un
  índice propio.
- Un pipeline para todos los tiers (`model.wgsl`): un uniform block (la `View`) y una textura
  leída con `textureLoad`, sin sampler, storage ni immediates (ADR 0011). El texel se recorta al
  rectángulo de su cara, así una cara nunca muestra la columna de la vecina en el borde.
- Una instancia se vuelve a posar solo si cambió su entrada (pose, lugar, luz, textura). Un
  frame donde nada cambió no sube nada; si solo se movió el origen de render, se reubican los
  vértices sin posar.
- Una llamada de dibujo por capa, material y textura. Sin culling por frustum ni por cuevas: son
  pocas instancias.

### 3. Dos capas

- `ModelLayer::World`: en el pase principal, después del terreno opaco y cutout y del LOD, antes
  del contorno y del agua. Prueba y escribe profundidad como el terreno: el agua delante lo tiñe
  y la niebla de `FogPlugin` lo ve en la profundidad.
- `ModelLayer::View`: relativo a la cámara, después de la niebla, sobre una profundidad limpia.
  Es la mano de primera persona: nunca se mete en una pared.

### 4. Luz como el terreno

- Una `LightValue` por instancia: la del bloque de una sonda (`ModelLight::Probe`, por defecto el
  origen; el juego usa los ojos) o una fija. Cielo abierto donde no hay luz cargada.
- El fragment usa la curva de brillo, el color del cielo de la hora, la ganancia de la luz de
  bloque y el piso de ambiente de los chunks, sin AO, y después la niebla de distancia.
- El sombreado por cara es el de las entidades de Minecraft: dos luces direccionales fijas,
  `0,4 + 0,6 * (n·l0 + n·l1)`, calculado en la CPU con la normal posada. La capa de la vista se
  sombrea con la normal vista desde la cámara.

### 5. Texturas del juego o del pack

- El juego da RGBA. `boxel_pack::entity_texture` lee una textura por id (`textures/<ruta>.png`,
  con overlays), por ejemplo una piel de jugador de un pack.
- `ModelTexture::from_pack_or` usa la del pack si tiene las proporciones de la del juego (o un
  múltiplo entero, un pack HD), y si no la del juego, con un aviso.

### 6. ECS

- `ModelInstance` (modelo, textura, pose, capa, origen, escala, luz) con el `Transform` de la
  entidad. El render plugin las copia en su `Extract` y las pasa a
  `VoxelRenderer::set_models`.
- El renderer crea los pipelines con la primera instancia. Sin instancias no hay pipeline, buffer
  ni pase: los frames son los de antes, byte a byte.

### 7. Costo medido

En la CPU (`benches/models.rs`, un modelo como el jugador de Minecraft: 12 cajas, 72 quads):

| Caso | 1 instancia | 50 instancias |
|---|---:|---:|
| todas con pose nueva (animadas) | 4,6 µs | 266 µs |
| solo se movió la cámara | 0,25 µs | 25 µs |
| nada cambió | 0,04 µs | 1,9 µs |

En la GPU no se nota: `just perf-smoke --base HEAD "--new-extra='--models 50'"
rtx/surface/r16/uncapped` (50 figuras caminando delante de la cámara, posadas en cada frame) dio
1926 fps sin ellas y 2013 con ellas, p99 1,33 y 1,16 ms: ruido. Ese caso está limitado por la
GPU, así que el trabajo de la CPU se solapa con la espera. Con 50 instancias animadas se suben
unos 460 KB por frame.

## Consecuencias

- El juego dibuja su jugador y después sus mobs con el mismo mecanismo. Las animaciones son del
  juego: el motor solo dibuja la pose que le dan.
- Muchas instancias (cientos de mobs) pedirían posar en la GPU (matrices por parte) y cull por
  instancia. Queda para cuando haga falta.
- No hay modelos translúcidos, sombras ni objetos en la mano todavía.
