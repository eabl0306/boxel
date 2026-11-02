# ADR 0062: luces dinámicas

- Estado: aceptado (issue #10, plan de luz P8)
- Fecha: 2026-11-02

## Contexto

La luz de bloque está horneada en las mallas (ADR 0015). Una antorcha en la mano que la usara
remallaría chunks en cada paso. El dueño pide luces que se mueven y no son bloques, aplicadas en
el shader, en los tres tiers.

Lo que pesaba:

- **Low** tiene un solo uniform block (ADR 0011). Todo lo nuevo va en el `View`.
- **Los tres tiers** dan los mismos píxeles.
- **Sin luces**, un frame da los mismos píxeles que main y casi no paga nada.
- **La elisión de AO** (ADR 0015, sección 6): una cara con sus 4 esquinas a oscuras lleva AO
  abierto. Una antorcha en la mano ilumina esas caras sin remallarlas.

## Decisión

### Qué es una luz

- `boxel_render::DynamicLight { emission: [u8; 3], offset: Vec3 }` en una entidad con
  `Transform`: brilla desde su traslación más `offset`, con un nivel por canal de bloque (rojo,
  verde y azul, `0..=15`) como la emisión de un bloque. Con emisión 0 no brilla ni cuesta.
- Cada frame el plugin de render toma las 16 más cercanas a la cámara (`MAX_DYNAMIC_LIGHTS`) y
  las pone en `SceneLight::dynamic` (`DynamicLights`).

### Cómo llegan al shader

- **Uniform**: `View` suma `dynamic_count` (`vec4<u32>`), `unproject` (la inversa de
  `view_proj`) y 16 `DynamicLight` de 2 `vec4` (posición relativa al `render_origin` y alcance;
  nivel por canal). Son 592 bytes: `View` pasa de 816 + 8192 a 1408 + 8192 bytes (9600), un solo
  uniform block en Low, lejos de los 16 KiB. Los tres tiers leen el mismo uniform con el mismo
  código.
- **Vértice**: cada quad calcula qué luces pueden llegar a la caja de sus celdas (con 2 bloques
  de margen) y guarda una máscara de 16 bits en los bits libres 16 a 31 de `VsOut::ao`. Es la
  misma en sus 4 esquinas. Sin luces el bucle no corre y la máscara vale 0.
- **Fragmento**: si la máscara vale 0 hace lo mismo que antes. Si no, saca su posición de su
  posición en el framebuffer y su profundidad con `unproject`, y recorre solo las luces de la
  máscara. No hay varying nuevo: la primera versión llevaba la posición en un varying y costaba
  18 a 24 % de los fps sin ninguna luz (abajo). Los quads de patrón ya leen la posición en el
  framebuffer igual en los tres tiers (ADR 0056).
- **Celdas** (`cell.wgsl`): igual, con la máscara por vértice (un quad de celda mide un bloque
  como mucho) en los bits altos de su varying de reino.
- **Modelos**: en la CPU. Un modelo con sonda (ADR 0040) sube cada canal de su luz al nivel de
  las luces en la sonda, redondeado. Uno con luz fija no cambia. Así la mano en primera persona
  toma la luz de la antorcha que sostiene.
- **LOD**: no. Está más allá de la distancia de vista y una luz llega a 14 bloques.

### Caída y color

- Una luz de nivel `L` da `L - d` a distancia de Manhattan `d`: es como cae la luz de un emisor en
  aire abierto. `d` se mide desde el punto medio bloque delante de la cara, el centro del vóxel
  del que la cara lee su luz. Una luz en el centro de un vóxel da en las caras los mismos niveles
  que un bloque emisor ahí. Medido en `gpu_dynamic_lights`: contra una lámpara blanca puesta en el
  mismo vóxel, 0,27 niveles de 255 de diferencia media por canal.
- RGB por canal, como la luz de bloque. El cielo no cambia.
- Una cara no toma luz de detrás de su plano. Las cruces (plantas) no tienen lado y toman la luz
  en su punto.

### Mezcla con la luz horneada

Por canal, el máximo entre la luz horneada interpolada y la de las luces, antes de la curva de
brillo, como se mezcla la luz de bloque. La curva sube siempre, así que da lo mismo que el máximo
después. Una antorcha en la mano se ve como una puesta.

### AO bajo la luz

La elisión no sirve con luces dinámicas: una cara oscura iluminada por la mano se ve sin AO. Es
correcto solo con AO en toda cara oscura que una luz pueda alcanzar, y una luz va donde va una
entidad, así que sin saber dónde están las luces son todas.

- **Regla**: un mundo con luces dinámicas malla con `AoPolicy::Baked`. El motor no cambia su
  valor por defecto (`BakedLitOnly`): sin luces, nada cambia. El juego usa `Baked` con la luz
  suave encendida.
- **Costo** (`headless_world`, overworld, radio 16):

| Vista | `BakedLitOnly` | `Baked` |
|---|---|---|
| superficie (y = 80) | 5,02 M quads | 8,50 M (+69 %) |
| cuevas (y = -1000) | 9,05 M quads | 16,76 M (+85 %) |

  Las paletas no cambian. Bajo tierra el arena por defecto (8 Mi entradas) ya no alcanza y crece
  (ADR 0035).
- **Una regla más barata y correcta**, para después: AO en las caras oscuras solo en los chunks
  cerca de una luz, remallados cuando una luz se acerca, con margen para que llegue antes que la
  luz. Toca el streaming (las opciones de malla por chunk), así que la decide el dueño.

### Aproximaciones

- Sin sombras: la luz pasa las paredes hacia las caras que la miran (el suelo del cuarto de al
  lado). Las que miran hacia otro lado no la toman.
- Detrás de un pilar o una esquina queda más oscuro que con una antorcha puesta, que rodea por
  inundación.
- Un modelo toma una sola luz por instancia, en su sonda.
- Más de 16 luces: quedan las 16 más cercanas a la cámara.

### Costo

- Sin luces: una prueba de bits por fragmento y un `if` por vértice. Medido abajo.
- Con luces: por vértice, una prueba de caja por luz; por fragmento, solo los quads al alcance, una
  cuenta por luz de su máscara.

## Medidas

Se completan en la entrega (perf-smoke del motor y del juego).

## El juego

La mano (y la otra mano) con un ítem de luz brilla desde el ojo con la luz del bloque que pone:
antorcha, antorcha de almas, farol, piedra luminosa, cubo de lava y cualquier otro que emita, el
máximo por canal de las dos manos, como los mods de luz dinámica de Minecraft. Los ítems tirados y
las entidades todavía no se dibujan, así que por ahora solo la mano.

## Consecuencias

- Un mundo con luces dinámicas paga AO en las caras oscuras: más quads, más arena.
- Las luces no remallan nada ni suben nada a la GPU salvo el uniform de cada frame.
- `View` crece 592 bytes por frame.
