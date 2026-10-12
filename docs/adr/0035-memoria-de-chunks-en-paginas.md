# ADR 0035: la memoria de chunks en páginas

- Estado: aceptado. El dueño tomó las recomendaciones del plan el 2026-10-12 (issue #57, plan de
  crecimiento del arena). Primera parte (M0 y M1): las páginas con capacidad fija y la sonda de
  asignación. El crecimiento (M2) y el encogimiento (M3) vienen después.
- Fecha: 2026-10-12

## Contexto

Los quads de los chunks vivían en un solo buffer de tamaño fijo. Con radio 32 bajo tierra hacen
falta unos 33 M de quads (270 MB) y con 24 Mi se perdían 13 211 chunks (issue #20). Un buffer
no crece en wgpu: crecer obliga a copiar todo a uno nuevo y, durante la copia, a tener los dos.
En la 780M eso es memoria del sistema. Además High dibujaba como mucho 65 535 buckets por lista
y descartaba el resto sin orden.

## Decisión

### 1. Páginas de tamaño fijo

Los quads viven en varios buffers del mismo tamaño, las páginas: 4 Mi quads (32 MiB) por
defecto. Las del piso pueden ser más chicas (punto 6). Cada página tiene su propio `offset_allocator` con nodos `u32` (el modo `u16` corta los
offsets mayores a 65 535). Un chunk entero va en una página: sus 8 buckets y su paleta de luz. Así
`quad_base` y `light_base` son locales a la página y los shaders de vértices no cambian. En Low
cada página tiene una página de luz paralela.

`ChunkGpu` guarda la página en el campo que antes era relleno. Sigue en 64 bytes.

### 2. Dos clases

Una página es Solid o Translucent. Los chunks con quads translúcidos van a las Translucent, así el
pase translúcido, que tiene un solo orden en los tres tiers, casi no cambia de página. Si su clase
no tiene lugar, un chunk usa la otra antes de rechazarse: la imagen es la misma y solo cuesta un
bind más.

### 3. Dónde va un chunk

La misma regla en los tres tiers: su página actual, después las páginas de su clase de menor id,
después su mismo rango en el lugar, después la otra clase. Agregar una página vacía no mueve a
nadie.

### 4. Una página es una lista de High

Una página tiene a lo sumo 10 922 chunks, así que sus 6 buckets de cara entran en una lista de
65 532 draws. High hace 2 binds y 2 multi-draws por página con chunks. Ya no descarta buckets.
Mid y Low dibujan página por página, en orden de slot dentro de la página, y atan la página antes
de sus draws.

### 5. Empates translúcidos por origen

Los chunks translúcidos a la misma distancia van por su origen `i64`, no por slot. El orden queda
igual tras un resync o al reabrir un mundo.

### 6. Ajustes

`ChunkMemorySettings` reemplaza `arena_quads` y `max_chunk_slots`: el piso (`initial_quads`,
8 Mi por defecto), las slots iniciales, el techo en quads y slots (igual en los tres tiers; los
bytes solo se informan) y el tamaño de página. El techo automático es 192 Mi quads en una GPU
dedicada, 96 Mi en una integrada y 32 Mi en otras, y nunca queda por debajo del piso pedido: el
backend GL de wgpu informa casi toda GPU real como `Other`, y antes un piso de 48 Mi quedaba en
32 Mi sin aviso. Si el piso igual se corta (por el límite de páginas), se avisa al arrancar.

El piso tiene lugar para los chunks de todas las slots iniciales, como el arena único de antes.
Si sus quads dan menos páginas de las que hacen falta (una cada 10 922 slots), los mismos quads
se reparten en más páginas, más chicas, de 4 096 quads como mínimo. Por defecto son cuatro
páginas de 2 Mi: 64 MiB, igual que antes. Si el techo no deja tantas páginas, se avisa al
arrancar. Un chunk que no entra porque las páginas con quads libres ya tienen todos sus chunks da
su propio error, `PageChunksFull`, y las estadísticas cuentan esas páginas.

### 7. Lo que se escribe pasa por un sumidero

Toda escritura de memoria sale como `MemOp` hacia un `CopySink`. En la GPU se agrupa en buffers de
staging y copias. En los tests se aplica sobre modelos en bytes de cada página y de la tabla, y un
proptest comprueba frame a frame que lo que hay en la GPU es lo que dice la CPU.

## Pendiente

- La sonda (`stress --alloc-probe 32,64`) está hecha pero no se corrió: sus números deciden si la
  página queda en 32 MiB o pasa a 64.
- Crecer, encoger y reintentar los chunks rechazados (M2 y M3). Hasta entonces el piso es todo lo
  que hay.

## Consecuencias

- Los tests de GPU comparan muchas páginas chicas contra una sola: la imagen es idéntica en cada
  tier, también lejos del origen.
- Entre chunks vecinos, Mid y Low pueden dibujar en otro orden que antes. Solo se nota en píxeles
  de borde con profundidad exactamente igual, dentro de la tolerancia entre tiers.
- La tabla de chunks y las páginas tienen `COPY_SRC` para moverse y leerse en tests.
