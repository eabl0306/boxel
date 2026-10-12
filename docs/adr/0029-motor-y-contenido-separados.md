# ADR 0029: el motor sin contenido; el contenido de ejemplo en `boxel_demo`

- Estado: aceptado (issue #48)
- Fecha: 2026-10-10
- Revisa: ADR 0018 (modelos), ADR 0020 y 0027 (escaleras, vallas, antorchas, muros y puertas de
  valla), ADR 0021 y 0025 (fluidos), ADR 0028 (texturas animadas de los packs)

## Contexto

El motor traía un juego de bloques estilo Minecraft dentro de sus crates.

- `BlockRegistry::default_blocks()` y sus texturas procedurales.
- Variantes de modelo con nombre de bloque: `Stairs`, `Fence`, `Torch`, `WallTorch`, `Wall` y
  `FenceGate`.
- Reglas deducidas del modelo.
- Agua y lava buscadas por nombre (`boxel:water`).
- Las recetas de los packs.
- Los generadores de mundo.
- `WorldPlugin` y el render caían en ese contenido por defecto.

Un juego que no es Minecraft no podía usar el motor sin cargar todo eso.

## Decisión

### 1. Qué queda en el motor y qué sale

Queda lo que cualquier juego de vóxeles usa con sus propios bloques:

- estados de bloque;
- modelos `Cube`, `Slab`, `Cross`, `Fluid` y `Boxes`;
- colisión y contorno por cajas;
- el mecanismo de reglas de vecinos y de uso;
- los ticks programados;
- el algoritmo de fluidos;
- luz, malla, render, persistencia y físicas;
- el ruido;
- `WorldGenerator` y `FlatGenerator`;
- un set mínimo de bloques de prueba (`boxel_voxel::testing`, con la feature `testing`, nombres
  `test:`).

Sale a `crates/boxel_demo` (`publish = false`):

- los bloques, sus texturas y el pintor procedural;
- las formas y reglas de Minecraft;
- el agua y la lava con sus reacciones;
- las recetas de packs y los tintes;
- los generadores bosque, terreno, overworld, mar y cuevas.

### 2. El registro describe el contenido; el motor no adivina

- **Formas.** `BlockModel::Boxes(BoxModelId)` apunta a una tabla de `BoxModel { boxes, outline }`
  que registra el juego (`add_box_model`). La malla, la física y el contorno leen
  `registry.model_boxes(s)` y `registry.outline(s)`.
- **Reglas.** El juego registra una `NeighborRule` o una `UseRule` (closures) por bloque, con
  `set_neighbor_rule` y `set_use_rule`. `NeighborRules` y `BlockUses` las copian del registro y
  se pueden cambiar en tiempo de ejecución con `set`. Ya no se deduce nada del modelo.
- **Fluidos.** El juego declara cada tipo con `add_fluid(bloque, FluidRules)`, con:
  - quién lo reemplaza: `fluid_replaced_by`;
  - qué deja al caer sobre otro: `fluid_falls_into`;
  - en qué se endurece al tocar otro: `fluid_hardens` (puede ser junto a varios; gana el primero
    declarado);
  - cuál llenan los estados `waterlogged`: `set_waterlog_fluid`. Un bloque con agua nunca se
    endurece.

  `FluidPlugin` registra un tick por fluido declarado. `TickHandler` sigue siendo un puntero a
  función y lee la tabla del registro.
- **Texturas.** Cada capa es una imagen (`TextureSpec { name, image: TextureImage }`, con
  `binary_alpha` para las capas cutout). El registro exige un lado potencia de dos, igual en
  todas las capas. El pintor procedural es del contenido (`boxel_demo::textures`).

### 3. Sin defaults con contenido

- `WorldPlugin::default()` es un mundo vacío (solo aire, `FlatGenerator` sin capas) y avisa en
  el log.
- El render sin mundo usa una capa magenta.
- El smoke test usa una capa gris propia.
- `WorldPlugin::new(generador, registro)` es la forma normal de crearlo.

### 4. Recetas de packs

`boxel_pack::LayerRecipe` es un dato. Las recetas viajan con
`TextureSource::Pack(PackSource { dir, recipes })`. Sin recetas, una capa `ns:ruta` lee
`minecraft:block/ruta`. Una receta puede nombrar bloques y texturas del espacio de nombres del
propio pack. El flujo animado se busca por fluido: la textura de lado de cada fluido, en el
espacio 0 si no da luz y en el 1 si da luz.

### 5. Convenciones del motor que quedan

- La propiedad `level` 0..=15 de los fluidos.
- La propiedad `waterlogged`.
- La textura de lado de un fluido es su textura de flujo.
- Un fluido emisivo se desplaza a la velocidad lenta del shader y usa el segundo bit de "flujo
  animado por el pack".
- `MEMO_REACH` = 5: una pendiente más larga se lee sin memo y sigue exacta.
- `StateFlags::FLUID` solo para el modelo `Fluid` sin colisión.
- El nado por defecto (`SwimSettings`) está ajustado al agua.

### 6. `boxel_demo`

- Depende solo de `boxel_voxel`, `boxel_worldgen` y `boxel_pack`. Por eso los tests unitarios de
  los demás crates lo usan como dev-dependency sin tipos duplicados.
- Guarda los nombres `boxel:`, el orden de registro y cada número, para que los ids de estado,
  las capas, los píxeles y las partidas guardadas no cambien.

### 7. Chequeo en CI

`tools/no_content.py` falla en estos casos:

- si un crate del motor tiene `boxel_demo` como dependencia normal;
- si `boxel_demo` depende de algo más que esos tres crates;
- si fuera de los tests aparece un nombre `boxel:` que no está reservado (en código, en
  `format!` o en comentarios);
- si aparece un identificador de contenido conocido.

Los nombres reservados son `air`, `unknown`, las claves `format`, `states`, `generator`, `ticks`,
`clock`, `entities` y `body`, y el prefijo `boxel:`. No hay lista de excepciones: la que se usó
durante la migración solo podía perder líneas y quedó vacía.

## Cómo se probó

Antes de mover nada se fijaron hashes (G1 a G7) de lo que el motor hacía con el contenido de
serie, en una forma que no depende de cómo se representa un modelo:

- registro, estado por estado;
- texturas procedurales y con un pack sintético;
- tabla de cajas y mallas;
- luz;
- colisión;
- reglas de vecinos y de uso;
- 400 ticks de agua y lava.

La línea base B0 es `main` 21d16ec, con sus binarios guardados en
`bench-results/split48/b0/`. Sobre el árbol final:

- Los literales de G1 a G7 (13) y los de las huellas del worldgen (33) siguen iguales, letra por
  letra.
- Las 234 capturas (18 escenas, 3 tiers, procedural y dos packs) tienen el mismo md5 que en B0,
  sin reintentos.
- La partida que guardó B0 carga y se ve igual, y la que se guarda ahora es idéntica byte a byte.
- `headless_world` da los mismos chunks, quads y luz en todos los mundos.
- Están los 1246 tests de B0; 8 cambiaron de crate junto con su código.
- `just perf-smoke` contra el binario de B0 sale con 0.

Detalle y comandos en `bench-results/split48/integ/` (local, no se sube).

## Consecuencias

- Un juego que no es Minecraft registra sus bloques, formas, fluidos y reglas sin heredar nada.
- Migración para `game/`:
  - `WorldPlugin::default()` pasa a
    `WorldPlugin::new(gen, Arc::new(boxel_demo::blocks::registry()))`;
  - los generadores pasan a `boxel_demo::worldgen`; los bloques que ponen se eligen con
    `BlockNames` (en `TerrainSettings::names` y `OverworldSettings::names`, por defecto los
    `boxel:` de siempre), así que un juego los usa con sus propios bloques, y `check` dice qué
    nombres le faltan a su registro;
  - `--pack` pasa a `PackSource::new(dir).with_recipes(boxel_demo::pack::recipes())`, y los
    tintes a `boxel_demo::pack`;
  - `BlockModel::Stairs` y las demás variantes pasan a `Boxes` con un `BoxModel` registrado, y
    las formas a `boxel_demo::shapes`;
  - las reglas se registran por bloque; las del demo buscan a sus vecinos con
    `boxel_demo::rules::Kinds` y las propiedades del estado;
  - una puerta de valla abierta frena el movimiento con `set_blocks_motion`;
  - los fluidos se declaran en el registro (`boxel_demo::fluids::declare` declara agua y lava);
  - `StateFlags::FLUID` ahora significa solo "modelo `Fluid` sin colisión";
  - `TextureSpec` lleva una imagen: el pintor es `boxel_demo::textures` (`Pattern`, `paint`,
    `spec`).
- Pendiente:
  - velocidad de flujo por fluido (shader, zona protegida);
  - nado por fluido;
  - etiquetas de bloques;
  - un gancho genérico para colocar bloques;
  - que un juego extienda los `Kinds` del demo para bloques que agrega después de `builder()`.
