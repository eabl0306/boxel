# ADR 0048: datos por bloque y usos de varias celdas

- Estado: aceptado (el dueño aprobó EW-BE y EW-USE2 para el programa "todos los bloques",
  2026-10-31)
- Fecha: 2026-10-31

## Contexto

Un horno guarda sus ranuras y su fuego; un atril, su libro; un tocadiscos, su disco. El estado
del bloque no alcanza: cada ítem posible por cada ranura serían millones de estados. El juego
guardaba los hornos como un registro propio en los metadatos del mundo, aparte de los chunks.

Además una cama ocupa dos celdas y usarla o romperla toca las dos. La regla de uso cambiaba solo
la celda usada, y la regla de vecinos de un bloque corre sobre la celda editada: si solo cambia
una mitad, su propia regla la vuelve atrás.

## Decisión

### Datos por celda (EW-BE)

- `BlockData` (en `boxel_voxel`): campos con nombre, ordenados. Cada valor es un número
  (`Int`), un texto, bytes del juego o un inventario (`Items`: una ranura por entrada, vacía o
  con un `ItemStack`). El ítem va por su nombre (`game:coal`), así una partida guardada no
  depende de los ids del juego. El motor no nombra contenido: los campos son del juego.
- Los datos viven en el `ChunkData` del chunk, por celda (`block_data`, `set_block_data`,
  `block_data_cells`). Viajan con el snapshot: se guardan, se cargan y se descargan con el
  chunk sin tocar el streaming. Un chunk sin datos lleva un puntero vacío.
- Cambiar datos no sube `version`: los bloques son los mismos y un trabajo de malla en vuelo
  sigue valiendo.
- Se escriben por la cola de ediciones, en el mismo lote que los bloques
  (`EditQueue::set_data`, `keep_data`). El lote aplica primero los bloques y después los datos,
  así un bloque puesto con sus datos en un frame los tiene. `EditQueue::pending_data` dice lo
  que va a quedar, para que dos sistemas del mismo frame no se pisen.
- Reglas al cambiar el bloque:
  - otro estado del mismo bloque conserva los datos (un barril que se abre);
  - otro bloque los pierde, salvo que el lote tenga un dato para esa celda (`keep_data`: un
    cofre de cobre que se oxida);
  - el aire nunca tiene datos.
- Lectura por posición `i64`: `ChunkMap::block_data_at` y `TickContext::block_data`.
- Una celda con datos marca su chunk como editado: se guarda aunque ningún bloque haya cambiado.

### Formato

- El registro de chunk sin datos se escribe igual que antes (versión 1, mismos bytes). Con
  datos es la versión 2: el almacenamiento y después las celdas (índice local creciente, campos
  por nombre creciente, cada valor con su etiqueta). `decode_chunk` lee las dos.
- Los mundos viejos cargan sin cambios. Un motor viejo no lee un chunk de versión 2: lo cuenta
  como error de carga y lo genera de nuevo, como cualquier registro que no entiende.
- Datos dañados o fuera de orden son un error, nunca un pánico (tests con cada bit y bytes al
  azar).

### La costura para E8 (ítems dibujados en la malla)

- `BlockRegistryBuilder::set_meshes_data(block)` marca un bloque cuya malla lee sus datos
  (`StateFlags::MESHES_DATA`). Un cambio de datos de ese bloque pide malla nueva de su chunk
  como una edición (época nueva, el mismo camino que un bloque). Los demás datos no remallan.
- El mesher ya recibe el `ChunkData` del centro (`MeshInput::center`) y con él sus datos: leer
  `block_data_cells` no copia nada. E8 agrega un paso después de los bloques especiales que
  recorre esas celdas y hornea el ítem en los quads del chunk.
- Lo que E8 tiene que cuidar: `trivially_empty` da malla vacía a un chunk uniforme sin mirar
  datos (un chunk lleno de marcos no es uniforme, pero conviene que el paso lo compruebe), y el
  ítem de una celda solo se dibuja dentro de su chunk.

### Usos y colocación de varias celdas (EW-USE2)

- `UseRule::cells`: la regla lee las celdas de alrededor (`Use::at`) y devuelve los cambios
  como desplazamientos desde la celda usada. `BlockUses::use_cells` los da como ediciones de un
  lote. Una regla de una celda sigue igual y responde también a `use_cells`.
- `PlaceRule`: elige los estados al colocar desde el clic (cara, punto, bloque clicado) y la
  mirada, y puede llenar más de una celda (la cabecera de la cama). La registra el bloque
  (`set_place_rule`) y el juego la pide con `BlockUses::place`. Sin regla, el juego coloca a su
  manera. El juego elige la celda; la regla, los estados.
- Romper una mitad sigue por la regla de vecinos, como en Minecraft: la otra mitad, sin su
  pareja, pasa a aire en el mismo lote.

## Lo que no se hizo

- Ticks de datos en el motor: el juego recorre sus hornos. Hace falta un índice por chunk de
  las celdas con datos si algún día son muchos miles.
- Componentes de ítem (encantamientos, texto de un libro): `ItemStack` tiene nombre, cantidad y
  desgaste. Lo demás puede ir en bytes del juego.
- Las puertas siguen con su regla de una celda y su regla de vecinos (las dos mitades se usan en
  el juego): pasarlas a `UseRule::cells` no simplificaba nada.

## Consecuencias

- Un chunk sin datos no cambia: mismos bytes guardados, mismos quads (`headless_world`), un
  puntero más en `ChunkData`.
- Un chunk con datos se copia entero al escribir datos si un trabajo tiene su snapshot (como una
  edición). Con un horno encendido es una copia por frame mientras dura el trabajo: barato.
