# ADR 0034: abrir y cerrar mundos sin reiniciar

- Estado: aceptado. El dueño tomó las recomendaciones del plan el 2026-10-12 (issue #58, plan del
  ciclo de vida del mundo). M0: el store se cierra limpio. M1: el ciclo de vida y todos los
  reinicios. M2: las pruebas en GPU. M3: fugas, medición y esta ADR. M4 es del juego.
- Fecha: 2026-10-13

## Contexto

Para cambiar de mundo, el juego reiniciaba el proceso. Un menú de mundos necesita abrir otro
mundo, volver al título y abrir uno más, con la app en marcha y sin que nada del mundo anterior
llegue al siguiente.

## Decisión

### 1. Un mundo es datos

Un mundo es un `WorldSpec`: generador, `WorldSettings`, store (o ninguno) y reloj. Los plugins, el
registro de bloques, los codecs, los ejecutores, el pack, el tier y la GPU son de la app y no
cambian. Hay un solo mundo abierto a la vez.

### 2. Los recursos se quedan, el estado vuelve a cero

Todos los recursos del motor quedan insertados toda la vida de la app. Solo entra y sale el
recurso `WorldStore`, porque es el mundo. Cada plugin devuelve su estado por mundo al de un
proceso nuevo en tres schedules que corre el ciclo de vida:

- `WorldSave` (`SaveSystems::{Game, World}`): el último momento del mundo.
- `WorldTeardown` (`TeardownSystems`): despawn de las entidades `Saved` y `WorldScoped`, física,
  ticks, guardado, streaming, LOD, renderer y juego, en ese orden.
- `WorldOpen` (`OpenSystems::{Engine, Game}`): ticks y reloj guardados, el store del LOD, el
  jugador.

Cada recurso por mundo se registra (`world_resource`, con su prueba de vacío) y cada mensaje por
mundo también (`world_message`, el ciclo de vida lo limpia). Un test de censo falla si aparece un
recurso sin clasificar. `WorldEpoch` sube en cada apertura y cada cierre.

### 3. Cerrar

En un `PreUpdate`, el guardado y el desmontaje. El frame se declara como pausa
(`Time::declare_stall`) y no adelanta el reloj fijo. Después el mundo queda `Closing` unos
frames: el último guardado, el cierre del archivo y la liberación de la memoria van en el pool de
IO. Luego llega `WorldEvent::Closed` con un informe. Un open pedido mientras tanto espera a
`Closed`, así dos mundos nunca están en memoria a la vez. Si el último guardado falla por 10 s
(`PersistSettings::close_timeout`), se abandona y el informe lo dice.

### 4. Abrir

El mundo de `WorldPlugin::new` abre por el mismo camino, en `PreStartup`. Por eso un mundo abierto
después de otro empieza igual que en un proceso nuevo: bloques, luz, mallas, LOD, ticks, reloj e
imagen, una vez asentado. Solo el orden de los cuerpos de física puede cambiar.

### 5. El archivo se cierra limpio

`WorldStore::close` cierra el archivo redb bajo su propio lock aunque queden clones (M0). Después,
toda operación de un clon falla en silencio. El siguiente arranque abre el archivo al instante y
sin reparación.

### 6. El renderer olvida el mundo

El cierre sube `Resync::World` y `clear_world` deja el renderer como uno nuevo, con la memoria de
chunks en su piso (ADR 0035).

### 7. Los índices se arman al abrir el store

`WorldStore::open`, que el juego corre fuera del hilo principal, arma el índice de chunks
guardados y la semilla del LOD (sus ediciones y el índice por nodo). La apertura del mundo los toma
enteros y no recorre los chunks guardados en el hilo principal (M3, ver Mediciones).

## Lista para el juego

- Abrir el store fuera del hilo principal y leer ahí los datos propios (el jugador). Después pedir
  la apertura.
- Para abrir la carpeta que estaba abierta, esperar su `WorldEvent::Closed` (redb no la abre
  antes).
- Leer `Res<WorldStore>` solo con `world_open`.
- Crear las entidades del mundo en `OpenSystems::Game` y dar `WorldScoped` a las que no se guardan.
  Un `ChunkViewer` sin `WorldScoped` sigue cargando en el mundo siguiente.
- Nunca `DespawnOnExit` en una entidad `Saved`. Las entidades del mundo se despawnean en
  `TeardownSystems::Despawn`.
- No guardar clones de `WorldStore` fuera de los recursos del motor (tampoco en los plugins del
  juego).
- Escribir los datos propios con `WorldLifecycle::stage_meta` o en `SaveSystems::Game`.
- Armar `WorldSpec.settings` desde el `WorldSettings` vivo.
- Borrar un mundo solo después del `Closed` de un cierre con `save: false`.
- No guardar un `ChunkId` cuando cambia `WorldEpoch`.

## Mediciones (2026-10-13)

Detalle en [rendimiento/memoria-y-mundos-2026-10.md](../rendimiento/memoria-y-mundos-2026-10.md).

- El cierre en el hilo principal: 3,9 ms a radio 16 y 14,4 ms a radio 32, con todos los chunks con
  bloques guardados. Queda bajo los 20 ms del plan.
- La apertura de ese mundo guardado costaba 2,5 ms a radio 16 y 13 ms a radio 32, casi todo la
  semilla del LOD. Con los índices hechos en `WorldStore::open` cuesta 0,02 y 0,09 ms. Abrir el
  store, fuera del hilo principal, pasa de 1,3 a 4,2 ms y de 1,7 a 12 ms.
- Diez cambios de mundo volando a radio 16: el frame del cierre va de 2 a 5 ms en la RTX y de 4 a
  7 ms en la 780M, el de la apertura unos 2 ms. El mundo nuevo se asienta en 0,3 s. Fuera de esos
  dos frames, el p99 queda bajo 1,5 veces el p99 de la superficie.
- 30 mundos seguidos sin GPU dejan el heap, los archivos abiertos y los hilos como estaban.
- En los diez cambios volando, el heap de malloc en uso entre dos mundos crece unos 3 MiB en los
  primeros cambios y después unos 0,1 MiB por cambio, cada vez menos. La corrida lo lee en el
  frame de cada apertura.

## Pendiente

Las dos reglas de aceptación de la corrida de cambios no se cumplen. M3 no está cerrado hasta que
el dueño decida.

- El RSS crece con los cambios: tras diez, de un 5 a un 18 % en la RTX y de un 7 a un 9 % en la
  780M según la corrida, no el 5 % del plan. Casi todo es memoria libre que malloc guarda en sus
  arenas: crece unos 20 MiB, y el heap en uso unos 3. Con un solo arena (`MALLOC_ARENA_MAX=1`) el
  RSS crece un 3,5 %. Limitar los arenas en el juego u otro asignador lo bajaría. Una fuga de
  menos de 0,1 MiB por cambio no queda descartada. El dueño decide si esta medida reemplaza a la
  regla del 5 %.
- El plan pedía que ningún frame fuera de los dos de pausa pasara 1,5 veces el p99 de la
  superficie. El frame más largo pasa (de 4,6 a 5,2 ms en la RTX y de 8,5 a 8,8 ms en la 780M),
  pero el vuelo sobre la superficie sin cambios también tiene frames así (4,2 y 6,5 ms en 10 s). El
  p99 sí cumple.

## Consecuencias

- Cambiar de mundo no reinicia el proceso. Lo que necesita reinicio es lo de la app: registro,
  codecs, plugins, ejecutores, pack, tier y el tamaño de página de la memoria de chunks.
- Un mundo abierto después de otro reparte otros `ChunkId` y carga en otro orden frame a frame. Lo
  que se compara con un proceso nuevo es el mundo asentado.
- Un store abierto ya trae su índice de chunks guardados y la semilla del LOD. Sin el LOD, la
  semilla se suelta en el pool de IO al abrir.
- Las pruebas: tests del ciclo de vida sin GPU, tests de redb, fugas en un binario propio con un
  asignador que cuenta, censo de recursos y cambios de mundo en GPU en los tres tiers.
