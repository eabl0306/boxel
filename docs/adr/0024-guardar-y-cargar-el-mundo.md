# ADR 0024: guardar y cargar el mundo

- Estado: aceptado (issue #26). Sin mediciones todavía: ver "Lo que falta medir".
- Fecha: 2026-10-09
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

### 3. Entidades: después

Los cuerpos congelados de la física (ADR 0016) siguen sin guardarse. El motor no tiene todavía
una forma de serializar componentes, y la física se está cambiando en otra rama. La propuesta
para después:

- Un trait `EntityCodec` que pone el juego: qué entidades se guardan (un marcador `Saved`) y cómo
  pasan a bytes y vuelven.
- Una tabla nueva `entities` con la misma clave de chunk: al descargarse un chunk se guardan y se
  despawnean las entidades que están adentro; al cargarse vuelven, dormidas o congeladas como
  estaban. Una tabla nueva no cambia la versión del formato (los que no la conocen la ignoran).

### 4. Formato (versión 1)

- **Archivo**: un directorio por mundo con `world.redb` (`WorldStore::open_dir`). Tres tablas:
  `chunks` (clave de 27 bytes, valor el registro), `index` (las mismas claves solas, escritas en
  la misma transacción: abrir el mundo lee unos bytes por chunk y no todos los registros) y
  `meta` (nombre, valor). La caché de páginas de redb queda en 64 MiB (su valor por defecto es
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
  aviso), `boxel:ticks` y `boxel:clock`.

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
    Lo mismo al salir.
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
- `voxel_world --world-dir DIR`: guarda al salir y carga al arrancar (también la posición de la
  cámara). `--camera` y `--time` mandan sobre la cámara y la hora guardadas. Si el mundo se creó
  con otro `--world` o `--seed`, se niega a abrirlo y dice con cuáles.
- Línea `save` del `DiagnosticsBoard` (la escribe `LogDiagnosticsPlugin` en el log): guardados,
  cargados, en cola, pendientes y errores.

## Consecuencias

- Editar, salir y volver a entrar deja el mundo igual: bloques, luz, agua que corre y hora.
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
- **Durabilidad diferida de redb (`Durability::None`)**: menos escrituras al disco, pero un cierre
  inesperado pierde chunks que ya se habían soltado de memoria.
