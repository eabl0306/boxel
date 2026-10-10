# ADR 0024: guardar y cargar el mundo

- Estado: aceptado (issue #26). Sin mediciones todavía: ver "Lo que falta medir".
- Fecha: 2026-10-09; la sección 3 (entidades), 2026-10-10.
- Revisa: la sección 12 de `docs/architecture.md` (el trait `ChunkStore`), el punto de la
  descarga de ADR 0015 sección 7 (una edición que movió un techo ya no se pierde) y "Fuera de
  esta parte" de ADR 0021 (los ticks programados se guardan).

## Contexto

Hasta ahora solo existía el trait `ChunkStore` con `NullChunkStore`: el mundo se regeneraba
desde la semilla y una edición se perdía al descargarse su chunk. La luz lo compensaba volviendo
a caminar la columna de un chunk editado que se iba (ADR 0015), y los ticks del agua que
esperaban en un chunk descargado corrían sobre el chunk regenerado (ADR 0021).

Lo que pedía la issue y el plan de M1 (`docs/architecture.md` sección 12):

- redb (Rust puro, ACID, una transacción de escritura a la vez) y un IdMap de nombre a id.
- Guardar los chunks editados, lo que la luz necesita por columna y las entidades.
- Claves con los 64 bits de cada eje (ADR 0014) y un formato estable y versionado.
- Nada de IO en el hilo principal y nada de tirones al cargar.

## Decisión

### 1. Qué se guarda

- **Chunks editados.** Un chunk se guarda solo si una edición lo cambió (las del jugador, las de
  los ticks y las reglas de vecinos: todas pasan por `apply_edits`). Un chunk generado que nadie
  tocó nunca se guarda: vuelve del generador, que es determinista. Un chunk leído del disco y no
  editado tampoco se vuelve a escribir.
- **Ticks programados pendientes** (`BlockTicks`, el agua que corre): todos, los de chunks
  cargados y los congelados, como `(posición, ticks de juego que faltan)` en el orden en que
  corren. Al arrancar se programan otra vez relativos al reloj de esa partida
  (`BlockTicks::pending` y `BlockTicks::restore`), así el reloj de los ticks no se guarda y el
  agua sigue igual que si no se hubiera cerrado el juego (test).
- **El reloj del mundo** (`TimeOfDay::tick`). Al arrancar, una partida guardada vuelve a su hora.
  Un juego que fija la hora por su cuenta apaga `PersistSettings::clock` y el reloj ni se guarda
  ni se restaura (`voxel_world --time`).
- **Datos del juego**: `WorldStore::meta` y `WorldStore::put_meta` guardan valores con nombre
  (la posición del jugador, la semilla). Los nombres que empiezan con `boxel:` son del motor.

### 2. Lo que la luz necesita: nada guardado aparte

El plan de ADR 0015 era guardar "los techos de cielo editados por columna". Se decidió no
guardar techos: el techo de una columna es un dato derivado del contenido, como la luz, y si se
guardara aparte podría quedar distinto de los chunks guardados (un cierre entre dos escrituras).
En cambio la caminata de una columna ve el contenido guardado:

- **Desde dónde camina.** Antes empezaba en el tope del generador o en el bloqueador editado más
  alto de los chunks cargados. Ahora empieza también, como mínimo, en el tope del chunk guardado
  más alto de la columna (el índice de chunks guardados lo sabe sin leer el disco). Un bloque
  puesto muy arriba del terreno sigue tapando el cielo aunque su chunk no esté cargado.
- **Qué lee.** Los chunks cargados del snapshot, como siempre; los guardados que no están
  cargados, del disco (dentro del job de la caminata, en un hilo de trabajo); el resto del
  generador. Donde una columna puede llegar al vacío, los chunks guardados por debajo del piso
  cuentan como contenido debajo (`content_below`).
- **Descarga.** Con persistencia, un chunk cuya edición movió un techo ya no manda su columna a
  caminar de nuevo: la edición quedó guardada y el techo sigue valiendo.
- **Reescaneo.** Al quitar el bloque del techo, la búsqueda del siguiente bloqueador hacia abajo
  no salta como aire un chunk guardado que no está cargado: manda la columna a caminar.

Resultado: los techos de un mundo reiniciado son iguales a los de antes de cerrar, también los
que salen de un chunk guardado que no está cargado (test, cerca del origen y en 2^40).

### 3. Entidades

Actualizado el 2026-10-10 (rama `feat/persist-bodies`). Antes los cuerpos congelados de la física
(ADR 0016) no se guardaban: se perdían al salir y se quedaban vivos, congelados, en los chunks
descargados. Ahora se guardan con su chunk. `boxel_world` no puede depender de `boxel_physics`,
así que el mundo trae un mecanismo genérico y la física se engancha a él.

**El mecanismo (`boxel_world::persist`).**

- `Saved` marca una entidad que se guarda con el mundo. Al verla por primera vez, el mundo le da
  un id guardado (`SavedId`, un `u64`) que no se repite nunca, tampoco después de borrarla: el
  contador va en `meta` como `boxel:entities` y al abrir se toma el mayor entre él y los ids
  guardados más uno.
- Un `EntityCodec` es la parte de un plugin: un nombre estable (`boxel:body`), la versión de sus
  bytes, dónde está la entidad (un bloque), cómo escribe su parte y cómo la lee. Se registra con
  `add_entity_codec`, antes o después de `PersistPlugin`. Una entidad puede tener partes de
  varios codecs (la física guarda el cuerpo; un juego, su inventario). La coloca el primer codec
  que sabe dónde está; una entidad que nadie coloca no se guarda.
- Los ids de entidad del ECS nunca se guardan: una entidad que vuelve es una entidad nueva. Un
  codec que guarda una referencia a otra entidad guardada guarda su `SavedId`, y al leerla la
  busca entre las vivas (`SavedIds`). Las entidades de un chunk vuelven todas antes de que corra
  ningún codec, así las referencias dentro de un chunk se resuelven. Una referencia a una entidad
  que no está viva se pierde.
- Un codec dice además si lo que guarda de una entidad pudo cambiar desde un tick de cambio
  (`EntityCodec::changed`, por defecto sí). La física lo responde con los ticks de cambio de los
  componentes que guarda.

**Qué guarda el disco.** Una tabla nueva `entities` (y `entity_index` con sus claves, como
`index`), con clave de 35 bytes: la clave del chunk (27, la de siempre) y el id guardado (8, big
endian). Las entidades de un chunk son un rango de la tabla. Cada entidad tiene un solo registro,
bajo el chunk en el que estaba al guardarse: una entidad que se movió se escribe bajo su chunk
nuevo y su registro viejo se borra en la misma transacción. Así el disco nunca tiene una entidad
dos veces ni ninguna, tampoco después de un cierre inesperado. Para eso un job de guardado toma
los registros como estaban cuando se armó, en el hilo principal: si la entidad se mueve mientras
el job espera, el job escribe lo que tomó y el siguiente escribe el movimiento (test con un job
retenido). Una tabla nueva no cambia la versión del formato: una versión del motor que no la
conoce la deja como está.

El registro es un sobre como los demás (magia `BXEN`, versión 1, suma de control y LZ4) con la
lista de partes: el nombre del codec, la versión de sus bytes y los bytes. El hilo principal arma
solo el contenido. El job de guardado lo sella y el job de generación abre los guardados, así la
suma de control y LZ4 no corren en el frame.

**Cuándo se guarda y cuándo vuelve.**

- **Guardar al descargar.** En `PreUpdate`, antes de los pasos fijos del frame, el paso de
  entidades busca las entidades guardadas cuyo chunk no tiene slot (se descargó, o la entidad
  quedó donde no hay nada cargado): escribe su registro y las despawnea. La regla mira el estado
  y no los eventos, y por eso no hay carreras: si un chunk se descarga y vuelve a crearse antes
  del paso, sus entidades siguen vivas.
- **Fuera de los límites verticales.** Fuera de `WorldSettings::vertical_bounds` no se crean
  slots, pero la física lee esas capas como aire: una flecha tirada por encima del límite de
  altura sube y vuelve a caer, como en Minecraft. Una entidad ahí va con la capa de su columna más
  cercana dentro de los límites: sigue viva mientras esa capa esté cargada, se guarda bajo ella y
  vuelve con ella (test). Antes se guardaba bajo un chunk que nunca carga y no volvía.
- **Un viewer no se guarda al descargar.** Una entidad guardada que es un `ChunkViewer` carga su
  propio chunk: guardada y despawneada, nada lo volvería a cargar (test). Se guarda con el
  autoguardado y al salir. Ningún codec guarda el viewer, así que un juego guarda a su jugador
  aparte (`WorldStore::put_meta`) o lo devuelve con un codec propio.
- **Volver al cargar.** El job de generación de un chunk que tiene entidades guardadas (lo dice
  un índice en memoria, armado al abrir con las claves) lee sus registros junto con los bloques,
  en el hilo de trabajo. Un chunk de una capa de aire que tiene entidades recibe un job. El paso
  de entidades del frame siguiente las devuelve, antes de que la física pase por ese chunk.
- **Nunca dos veces.** Un registro vuelve solo si su id no está vivo y si es el registro más
  nuevo de ese id (el índice sabe en qué chunk está cada id). Un chunk que se descarga y se
  vuelve a cargar en la misma partida no duplica nada, aunque el disco todavía tenga un registro
  viejo de una entidad que se movió.
- **Autoguardado y salida.** El autoguardado (cada `PersistSettings::autosave`) y la salida
  escriben las entidades guardadas vivas bajo su chunk actual, en el mismo momento y la misma
  transacción que los chunks con ediciones, los ticks y el reloj. Cuando el paso de guardado toma
  esa foto y hay entidades vivas, se la pide al paso de entidades (solo él corre los codecs), que
  en el `PreUpdate` siguiente entrega todo junto, sin pasos fijos en el medio; el job arranca en
  el frame siguiente (test). Antes iban con relojes distintos, y después de un cierre inesperado
  un cuerpo podía volver dormido en el aire, con bloques de otro momento.
- **Solo lo que cambió.** La foto rearma el registro de una entidad solo si algún codec dice que
  cambió desde que se armó o se leyó el suyo, o si ganó o perdió un componente. Un cuerpo dormido
  no cambia, así el costo sigue a lo que se movió y no a cuántas entidades guardadas hay (test).
  Lo mismo al guardar una entidad que se va con su chunk. De los registros que se rearman, solo
  se escribe uno distinto del anterior (huella FNV-1a del contenido).
- **Borrar.** Despawnear una entidad guardada, o quitarle `Saved`, borra su registro.
- **IO.** Los registros pasan por los jobs de guardado de siempre: cada job lleva todos los
  registros y borrados que esperan (son pocos bytes) y todos los chunks entregados, aunque pasen
  de `max_batch`, como un job con los ticks. Así un cuerpo nunca llega al disco antes que los
  bloques donde descansa (test).
  Mientras esperan quedan en memoria y las cargas los leen encima del disco, como las copias de
  chunks. Si el guardado falla, vuelven a la cola.
- **Registros malos.** Un registro dañado o de otra versión de registro no vuelve, se cuenta en
  `PersistStats::entity_errors` y queda en el disco como está. Una parte de un codec que no está,
  o que su codec no puede leer (otra versión), se guarda aparte y se vuelve a escribir tal cual
  con la entidad. Una entidad que ningún codec coloca no vuelve y su registro queda.

**Los cuerpos (`boxel_physics::BodyCodec`).** `PhysicsPlugin` lo registra siempre: un cuerpo
con `Saved` se guarda. Guarda el tipo, la posición exacta (`WorldPos`: los `i64` del bloque y el
desvío), la velocidad, `BodySettings`, el `Collider`, los `Contacts` del último paso, si duerme o
está congelado y cuántos pasos lleva quieto, y si los tiene: `Interpolated` (la posición antes del
último paso), `ReportContacts`, `CharacterController` con su `MoveIntent`, `Projectile` (el
tirador como `SavedId`, así solo se guarda un tirador guardado) y `Swimmer` (con los ajustes de
tierra que guarda mientras nada) con su `SwimIntent`. Los `f64` van bit a bit. No se guardan los
contactos entre cuerpos ni los eventos.

- **Congelado.** Un cuerpo `Frozen` vuelve congelado: el controlador y el nado no corren en un
  cuerpo congelado, y sin el marcador correrían una vez de más en el paso en que vuelve (test con
  un caminante en el borde del mundo cargado).
- **Una colocación pendiente.** Un `Teleport` pendiente, o un `Transform` escrito desde el último
  paso, se guarda como tal: el registro va bajo el chunk de ese lugar, y el cuerpo vuelve con el
  estado del último paso y un `Teleport` ahí, que el primer paso toma como lo habría tomado. Un
  cuerpo dormido teletransportado al aire se despierta y cae (test). Antes volvía dormido en el
  aire.

Al volver, el cuerpo entra ya colocado con todo su estado y un marcador interno `Restored`, que
dura hasta el final del `Wake` del paso siguiente y anota lo que había pasado al volver: el tick
de cambio, los lugares vaciados y el id del próximo evento de chunk. Lo de antes no lo toca: su
posición no es un empujón (despertaría a los que duermen alrededor), y las cargas de chunks y los
lugares vaciados anteriores no lo despiertan (su propio despawn al guardarse dejó uno justo ahí).
Lo que pasa después sí cuenta, aunque pasen frames sin paso fijo: un `BodyPosition` escrito es
un empujón, y la caja de abajo que se despawnea o un chunk que carga lo despiertan (tests). Un
cuerpo dormido vuelve dormido y sigue dormido; una pila de cajas no se despierta al cargarse.

Resultado: un cuerpo guardado y vuelto a cargar da los mismos pasos, bit a bit, que si nunca se
hubiera guardado (test con un reinicio a mitad de un salto, un deslizamiento y una caminata,
cerca del origen, en 2^40 y junto al límite del mundo). Esto vale para cuerpos que no se empujan
entre sí: los pares se ordenan por entidad y una entidad que vuelve tiene otro id.

`voxel_world`: C deja caer una caja (sin dibujo todavía; F3 cuenta las cajas y da la más cercana).
Con `--world-dir` las cajas se guardan con el mundo. El ejemplo registra su propio codec para el
marcador `Crate`, como haría un juego.

### 4. Formato (versión 1)

- **Archivo**: un directorio por mundo con `world.redb` (`WorldStore::open_dir`). Tablas:
  `chunks` (clave de 27 bytes, valor el registro), `index` (las mismas claves solas, escritas en
  la misma transacción: abrir el mundo lee unos bytes por chunk y no todos los registros),
  `meta` (nombre, valor), y `entities` y `entity_index` (sección 3). La caché de páginas de redb queda en 64 MiB (su valor por defecto es
  1 GiB).
- **Clave de chunk**: dimensión (`u16`), lod (`u8`) y x, z, y como `i64` en big endian con el bit
  de signo invertido (`format::encode_key`). El orden de los bytes es el de `(dim, lod, x, z, y)`:
  exacto para todo `i64` y con las capas de una columna juntas. El plan viejo de una clave `u128`
  con Morton no entra: tres ejes de 57 bits son 171 bits.
- **IdMap**: la lista de nombres de estado que el mundo usó (`boxel:log[axis=x]`, ADR 0019), en
  `meta` bajo `boxel:states`. Un bloque se guarda como el índice de su nombre. La lista solo
  crece: al abrir se agregan los estados del registro que faltan, así los ids de los chunks
  guardados nunca cambian de significado aunque el registro cambie de orden. Al abrir, cada nombre
  se resuelve contra el registro de esa partida:
  - un nombre que el registro conoce tal cual es ese estado, y su id;
  - un bloque conocido al que le falta una propiedad, o con una propiedad o un valor que ya no
    existe, conserva las propiedades que sí tiene y toma el valor por defecto del resto. Ese
    nombre no pasa a ser el id del estado que resultó: el estado se guarda con su nombre exacto.
    Si no, un bloque que ganó una propiedad se guardaría con el nombre viejo y cambiaría cuando
    cambie el valor por defecto (test);
  - un bloque que ya no existe es aire (con un aviso en el log y en `IdMap::unknown`).

  Como un registro leído no se reescribe, un bloque que vuelve en una versión posterior reaparece
  en los chunks que no se editaron mientras tanto.
- **Sobre** (todo valor del motor menos la versión): magia (`BXCK` en los chunks, `BXMT` en los
  metadatos), versión del registro, banderas, largo y una suma de control FNV-1a de 64 bits del
  contenido. El contenido va comprimido con LZ4 (bloque, `lz4_flex`) cuando eso lo achica.
  Un bit cambiado en cualquier byte o un registro cortado se detecta (test que prueba cada byte).
  Un largo dañado se rechaza antes de reservar memoria: no puede pasar del registro de chunk más
  grande ni de 255 veces el contenido comprimido (lo máximo que da LZ4). Los metadatos no tienen
  otro límite, así lo que se escribe siempre se puede leer (test con una lista de ticks de más de
  64 MiB).
- **Registro de chunk**: el almacenamiento compactado (`Paletted::compact`) con ids guardados:
  uniforme (un id), paleta (los ids, los bits por vóxel y las palabras `u64`) o directo (32 768
  ids). Un chunk de piedra con un hueco ocupa menos de 512 bytes.
- **Versiones**: `boxel:format` (hoy 1) cambia solo si los archivos viejos no se pueden leer: un
  mundo de otra versión no se abre y no se toca (`PersistError::Version`). Cada tipo de registro
  tiene su propia versión, para poder migrar registro por registro más adelante.
- **Otros metadatos**: `boxel:generator` (el nombre del generador con que se creó: otro solo da un
  aviso), `boxel:ticks`, `boxel:clock` y `boxel:entities` (el próximo id guardado).

### 5. Cómo corre sin frenar el frame

- **Abrir** (`WorldStore::open`, antes de arrancar la app): lee la versión, el IdMap y las claves
  de todos los chunks guardados, que arman el índice por columna (8 bytes por chunk). Es IO en el
  hilo principal, pero antes del primer frame.
- **Cargar.** Un chunk del índice no se genera: su job de generación lee el registro y lo
  decodifica (en un hilo de `AsyncComputeTaskPool`, como la generación). Un chunk de una capa que
  el generador declara de aire, que antes se creaba sin job, recibe un job si está guardado. Un
  registro dañado se genera, se cuenta en `PersistStats::load_errors` y se deja como está hasta
  que el chunk se edite. Si lo que falla es el disco y no el registro, el chunk no se genera:
  queda afuera (aire que no se puede editar) hasta que vuelva a cargarse. Así una copia inventada
  nunca se guarda encima del registro bueno (test).
- **Descargar.** Un chunk editado que se descarga se entrega a una copia pendiente en memoria
  (un `Arc` del snapshot y una entrada en un mapa: nada de copiar bloques ni de IO). Las cargas
  leen primero esas copias, así un chunk que vuelve antes de que su escritura termine trae sus
  ediciones (test).
- **Escribir.** Un job de guardado por vez, en `IoTaskPool` (no ocupa un hilo de chunks): toma
  hasta `max_batch` chunks (256), los codifica y los escribe en una transacción de redb con
  durabilidad inmediata. Cuando termina, suelta las copias que escribió, salvo las que tienen una
  copia más nueva. Por frame, el hilo principal solo entrega chunks y lanza como mucho un job.
- **Fallas.** Una escritura que falla deja sus chunks en memoria (las cargas los siguen viendo),
  los vuelve a poner en la cola y prueba otra vez después de `retry_after` (1 s). Se cuentan en
  `PersistStats::save_errors` (test con un disco que falla y vuelve). Después de un error de IO,
  redb falla en todo hasta que la base se abre de nuevo. Por eso `RedbStore` la cierra, abre el
  archivo otra vez (redb lo repara) y repite la operación. Mientras el disco sigue fallando lo
  intenta como mucho una vez por segundo (test con un archivo redb cuyo disco falla y vuelve).
- **Autoguardado** (`PersistSettings::autosave`, 60 s): entrega los chunks cargados con
  ediciones, los ticks y el reloj. Así un cierre inesperado pierde como mucho eso. Los chunks
  editados se llevan en un conjunto, no se recorre el mapa. El hilo principal solo copia la
  lista de ticks pendientes; el job de guardado la codifica y la comprime.
- **Un solo momento en el disco.** Los ticks y el reloj se guardan siempre en la misma
  transacción que todos los chunks con ediciones, cargados o entregados, y de ese mismo frame.
  Sin esto, un cierre inesperado mezclaba momentos: un chunk guardado al descargarse con agua a
  medio correr y los ticks del autoguardado anterior, y el agua quedaba trabada (test). Por eso:
  - esa foto se toma en el frame en que arranca el job, no antes (si hay un job corriendo, el
    autoguardado espera a que termine);
  - si hay ticks esperando, un chunk editado que se descarga no sale solo: su job es una foto
    completa, como el autoguardado (test con el agua en un chunk que sigue cargado);
  - un job que lleva los ticks lleva todos los chunks entregados, aunque pasen de `max_batch`.
    Lo mismo al salir;
  - si esa escritura falla, al reintentar se toma la foto otra vez: la vieja no se escribe junto
    a chunks más nuevos (test).
- **Salida** (schedule `Shutdown`, que corren los dos runners): espera el job en curso y escribe
  todo lo que queda en el hilo principal, que ya no dibuja.

La carga y la escritura usan la maquinaria de jobs de siempre (`JobExecutor`, con
`IoTaskPoolExecutor` nuevo para las escrituras), así los tests corren todo en línea y son
deterministas.

### 6. API

- `boxel_world::persist`: `ChunkStore` (ahora con `keys`, `load_meta` y `save_batch` de un
  `SaveBatch` de chunks y metadatos), `RedbStore`, `MemoryStore`, `NullChunkStore`,
  `WorldStore` (`open`, `open_dir`, `meta`, `put_meta`, `stats`, `ids`), `PersistPlugin`
  (después de `WorldPlugin`), `PersistSettings`, `PersistStats`, `PersistError`, `IdMap` y
  `format` (claves, registros).
- Un `WorldStore` sirve a un solo `PersistPlugin`. Una `App` nueva abre el mundo otra vez: un
  segundo plugin con el mismo store (o un clon) entra en pánico, porque empezaría sin el índice
  de chunks guardados y guardaría copias generadas encima de ellos (test).
- `ChunkSlot::dirty`: el chunk tiene ediciones que todavía no se entregaron para guardar.
- `BlockTicks::pending` y `BlockTicks::restore`.
- `jobs::IoTaskPoolExecutor`.
- Entidades: `Saved`, `SavedId`, `SavedIds`, `EntityCodec` (con `changed`), `EntityCodecs`,
  `add_entity_codec`,
  `format::EntityPart` (y `encode_entity`, `decode_entity`, `encode_entity_key`); en
  `ChunkStore`, `entity_keys` y `load_entities` (por defecto, un store sin entidades) y
  `SaveBatch::entities`; en `PersistStats`, `saved_entities`, `loaded_entities`,
  `entity_errors` y `pending_entities`. En la física, `BodyCodec`.
- `voxel_world --world-dir DIR`: guarda al salir y carga al arrancar (también la posición de la
  cámara). `--camera` y `--time` mandan sobre la cámara y la hora guardadas. Si el mundo se creó
  con otro `--world` o `--seed`, se niega a abrirlo y dice con cuáles.
- Línea `save` del `DiagnosticsBoard` (la escribe `LogDiagnosticsPlugin` en el log): guardados,
  cargados, en cola, pendientes y errores, también de entidades.

## Consecuencias

- Editar, salir y volver a entrar deja el mundo igual: bloques, luz, agua que corre, hora y
  cuerpos guardados.
- El índice de entidades guardadas (dónde está cada id) se arma al abrir con todas las claves y
  vive en memoria: unos 60 bytes por entidad. Con millones de entidades habría que leerlo por
  regiones, como el de chunks.
- Cada frame con una descarga y entidades guardadas vivas recorre esas entidades una vez (una
  búsqueda de chunk por entidad). Con miles de cuerpos guardados cerca conviene medirlo.
- Un cierre inesperado justo entre el job que guarda un chunk al descargarse y el del frame
  siguiente, que guarda los cuerpos que se fueron con él, deja esos cuerpos con su registro
  anterior (el del último autoguardado). Es una ventana de un frame; cerrarla pide guardar el
  chunk y sus cuerpos en el mismo job.
- Un mundo abierto con límites verticales más estrechos que los de cuando se guardó no devuelve
  las entidades guardadas bajo capas que quedaron afuera. Sus registros quedan en el disco.
- Un mundo guardado ocupa lo que ocupan sus ediciones, no lo que se exploró.
- Los ticks congelados de chunks que no vuelven siguen en memoria (como antes) y ahora también se
  guardan. Si fueran muchos, convendría guardarlos con su chunk.
- Mientras haya ticks esperando, cada job que guarda chunks descargados copia la lista de ticks
  en el hilo principal y vuelve a escribir los chunks cargados con ediciones. Guardar los ticks
  de cada chunk con su registro lo evitaría, pero cambia cómo `BlockTicks` guarda los ticks
  congelados.
- El índice de chunks guardados se lee entero al abrir: con un millón de chunks editados serían
  unos 32 MB de claves y quizá medio segundo. Para mundos así habría que leerlo por regiones.
- Un bloque que ya no existe se pierde (aire) en cuanto su chunk se edita y se guarda.
- Dependencias nuevas: `redb` 4.3.0 (MIT o Apache-2.0, sin dependencias) y `lz4_flex` 0.14.0
  (MIT, sin el formato de frame, así no trae `twox-hash`). `cargo deny check` pasa.

Tests (en `crates/boxel_world`):

- `persist/format/tests.rs`: claves ida y vuelta en 0, ±2^31, ±2^40, el límite del mundo y los
  extremos de `i64`, y su orden (proptest); registros de todos los almacenamientos; IdMap con un
  registro distinto, un bloque que ya no existe, una propiedad y un valor que ya no existen, y
  un bloque que ganó una propiedad cuyo valor por defecto cambia después; una lista de ticks de
  más de 64 MiB se lee de vuelta; cualquier bit cambiado o registro cortado es un error, y bytes al azar nunca entran en pánico.
- `persist/tests.rs`: metadatos de un mundo nuevo, nombres reservados, copias pendientes, el
  índice, el tamaño de los lotes (un job con los ticks lleva todos los chunks) y el orden de los
  ticks; un archivo redb cuyo disco falla y vuelve (guardar y cargar funcionan otra vez y no se
  pierde nada).
- `tests/persist.rs`: editar, descargar, volver y reiniciar la app, cerca del origen, cruzando
  ±2^31, en 2^40 y junto al límite del mundo, comparando los chunks vóxel por vóxel y que solo se
  guardan los editados (una vez más con los pools de tareas del juego, no en línea); guardado al salir y autoguardado; techos de cielo después de reiniciar,
  con un bloque guardado que no está cargado; agua que corre igual con un reinicio en el medio
  que sin él, también después de un cierre inesperado tras guardar chunks al descargarlos; el
  reloj; un registro dañado; otra versión y metadatos dañados; un estado que ya no
  existe; un mundo en redb en un directorio que no existía, un archivo que no es una base y las
  claves lejanas en redb; un chunk que vuelve antes de su escritura; un disco que falla y vuelve;
  un chunk que el disco no puede leer no se genera ni se guarda encima de su registro.

Tests de entidades:

- `persist/format/tests.rs`: claves de entidad ida y vuelta lejos del origen y su orden
  (proptest), registros ida y vuelta, cualquier bit cambiado o registro cortado es un error y otra
  versión se reconoce.
- `persist/tests.rs`: los dos stores guardan, leen por chunk y borran registros lejos del origen;
  los registros en memoria se leen encima del disco hasta que se escriben, y se escriben sellados;
  el índice al abrir, el contador de ids y un movimiento que borra el registro viejo; un guardado
  que falla en redb se escribe cuando el disco vuelve; una entidad que se mueve mientras su job
  espera nunca falta en el disco; un job con registros lleva todos los chunks entregados.
- `tests/persist_entities.rs` (con un codec de prueba): las entidades vuelven una sola vez
  después de descargarse tres veces, cerca del origen, cruzando ±2^31, en 2^40 y junto al límite,
  también en una capa de aire; vuelven después de reiniciar con los mismos ids y los ids nuevos no
  repiten; una entidad que se movió queda bajo su chunk nuevo y nada más; una despawneada pierde
  su registro; una puesta donde no hay nada cargado espera a su chunk; las referencias vuelven;
  registros dañados, de otra versión, de una versión de codec que no se lee y de un codec que no
  está quedan como están, también en una partida sin codecs; las entidades van en la misma
  transacción que los chunks y los ticks de su momento; el autoguardado solo rearma lo que
  cambió; una entidad por encima o por debajo de los límites verticales sigue con la capa de su
  columna, cerca y lejos; un viewer guardado nunca se guarda al descargar.
- `boxel_physics/tests/persist.rs`: cuerpos de todo tipo vuelven con todo su estado, una vez,
  cerca y lejos (también el `SwimIntent`); los dormidos siguen dormidos, también si pasan frames
  sin paso entre la carga y el paso; después de reiniciar dan los mismos pasos bit a bit, también
  un caminante congelado y un cuerpo con una colocación pendiente (`Teleport`, `Transform`,
  nuevo despierto o dormido); un cuerpo dormido no cambia para el autoguardado; lo que pasa
  después de volver (un despawn debajo, un empujón, un chunk que carga) lo despierta; un cuerpo
  que cruzó un borde queda bajo su chunk; otra versión del codec o bytes cortados quedan
  guardados como están.
- `voxel_world` (`shared/crates.rs`): una caja se guarda con sus dos partes y vuelve.

Imágenes: `snapshot` con ediciones, agua que corre y `--world-dir`, y después sin ediciones con el
mismo directorio, da los mismos píxeles de día y de noche, cerca del origen y en x = 2^40,
z = -2^31.

## Lo que falta medir

Hoy no se pudo medir (la máquina estaba ocupada). Toca caminos calientes: la creación de slots
(una búsqueda en el índice para las capas de aire), el job de generación (una búsqueda en el
índice), la descarga (entregar el chunk si está editado), las ediciones (marcar el chunk) y la
caminata (los chunks guardados de la columna). Sin `PersistPlugin` todo eso es una comprobación de
un `Option`. Con un mundo sin ediciones, el índice está vacío. Para la próxima ronda de control:

- `perf-smoke` de superficie y bosque con y sin `PersistPlugin` (un mundo vacío): no debería
  cambiar nada.
- Lo mismo con un mundo con muchas ediciones (por ejemplo una capa entera de chunks editados
  alrededor del origen), volando por encima: el peor frame no debería subir.
- Cuánto tarda un job de guardado de 256 chunks y cuánto ocupa un mundo de prueba en disco.
- El frame del autoguardado con mucha agua corriendo: copia la lista de ticks pendientes en el
  hilo principal (`BlockTicks::pending`), lineal en la cantidad de ticks. Lo mismo volando sobre
  chunks editados con agua corriendo, porque cada job de descarga hace esa copia.
- Entidades, medido (nivel 1, criterion, una corrida por lado, 2026-10-10): el paso de física no
  cambia con el marcador `Restored`. `scale/10000_awake` 1.809 ms en `main` y 1.786 ms en la
  rama; `scale/10000_asleep_chunk_events` 781 us y 761 us (ruido). Un registro de caja dormida
  ocupa como mucho 251 bytes (la parte del cuerpo son 218).
- Entidades, falta: el paso de entidades con miles de cajas guardadas dormidas cerca, volando para que
  sus chunks se descarguen y vuelvan (recorre las entidades vivas en cada frame con una descarga,
  codifica las que se van y cambiaron, y despawnea; al volver las crea en un solo frame). Si el
  frame que devuelve muchas sube demasiado, repartirlas en varios frames antes de que la física
  llegue a esos chunks. Sin entidades guardadas el paso no corre (una condición sobre el estado).
- Entidades, el autoguardado: ya no rearma las que no cambiaron (un cuerpo dormido no cambia) y no
  sella nada en el hilo principal. Falta medir su frame con miles de cuerpos despiertos.

## Alternativas descartadas

- **Guardar los techos de cielo por columna**: dato derivado que puede quedar distinto de los
  chunks guardados después de un cierre inesperado. Leer los chunks guardados da el mismo techo
  bit a bit y no guarda nada extra.
- **Guardar todo chunk que se cargó**: un mundo explorado ocuparía gigas y cada descarga
  escribiría. El generador ya los rehace.
- **Un hilo de IO propio**: el `IoTaskPool` ya existe para esto y los jobs se prueban con el
  ejecutor en línea.
- **Leer el disco antes de cada generación, sin índice**: una transacción de lectura por chunk
  creado y, peor, un job para cada chunk de aire (que hoy se crea sin job), que es la mayoría del
  mundo.
- **postcard o serde para los registros**: el formato propio es corto, no depende de cómo una
  librería codifica sus tipos y se prueba byte a byte.
- **Un registro por chunk con todas sus entidades** (el plan original): una entidad que se mueve
  de un chunk descargado a otro, o que se pone donde no hay nada cargado, obliga a leer y mezclar
  el registro del otro chunk, y una foto completa en cada descarga para no duplicar después de un
  cierre inesperado. Con un registro por entidad, mover es escribir uno y borrar otro en la misma
  transacción.
- **Ids de entidad guardados tal cual**: cambian en cada partida y se reciclan. El `SavedId` es
  estable y las referencias se guardan con él.
- **Leer las entidades en un job aparte al cargarse el chunk**: el chunk quedaría cargado un paso
  o más sin sus cuerpos, y la física pasaría por él. Leerlas en el job de generación las trae con
  los bloques.
- **Durabilidad diferida de redb (`Durability::None`)**: menos escrituras al disco, pero un cierre
  inesperado pierde chunks que ya se habían soltado de memoria.
