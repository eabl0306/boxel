# ADR 0035: la memoria de chunks en páginas

- Estado: aceptado. El dueño tomó las recomendaciones del plan el 2026-10-12 (issue #57, plan de
  crecimiento del arena). M0 y M1: las páginas y la sonda de asignación. M2 y M3: crecer y encoger
  mientras corre la app. Medido el 2026-10-13: la página queda en 32 MiB.
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
dedicada, 96 Mi en una integrada o de clase desconocida y 32 Mi en una de CPU o virtual. El
backend GL de wgpu informa casi toda GPU real como desconocida (`Other`), la 780M incluida, por
eso esas van como integradas. El techo automático nunca queda por debajo del piso pedido; antes
un piso de 48 Mi quedaba en 32 Mi sin aviso en GL. Si el piso igual se corta (por el límite de
páginas), se avisa al arrancar.

El piso tiene lugar para los chunks de todas las slots iniciales, como el arena único de antes.
Si sus quads dan menos páginas de las que hacen falta (una cada 10 922 slots), los mismos quads
se reparten en más páginas, más chicas, de 4 096 quads como mínimo. Por defecto son cuatro
páginas de 2 Mi: 64 MiB, igual que antes. El techo no limita ese reparto, porque los quads son los
mismos. Lo limitan los quads (4 096 por página) y las páginas que admiten las listas de High (84
con 10 922 chunks por página); si así quedan menos lugares que slots, se avisa al arrancar. El
piso nunca pasa del techo: si el techo no es múltiplo de página, el piso no redondea a páginas
enteras y reparte los quads del techo entre sus páginas. Un chunk que no entra porque las páginas con quads libres ya tienen todos sus chunks da
su propio error, `PageChunksFull`, y las estadísticas cuentan esas páginas.

### 7. Lo que se escribe pasa por un sumidero

Toda escritura de memoria sale como `MemOp` hacia un `CopySink`. En la GPU se agrupa en buffers de
staging y copias. En los tests se aplica sobre modelos en bytes de cada página y de la tabla, y un
proptest comprueba frame a frame que lo que hay en la GPU es lo que dice la CPU.

### 8. Crecer entre frames

Una política pura decide al final de cada frame, con un reloj que los tests adelantan. Cada clase
guarda una reserva de quads libres (media página Solid, un cuarto Translucent, o un octavo de lo que
usa si es más) y de lugares para chunks. Cuando le falta, pide una página. También pide una si un
chunk rechazado espera y no entra en ningún hueco.

- La página se crea justo después de present, en error scopes, una por frame. Nace en estado
  Clearing y los frames siguientes la limpian por partes (32 MiB por frame en una GPU dedicada, 8
  en una integrada, 4 en otra). Así wgpu nunca llena con ceros una página entera dentro de un frame.
  Si su id pasa las listas de High, llega con listas más grandes, que se limpian igual.
- Antes de colocar las mallas de un frame, `reserve` hace lugar: pone en uso una página que se
  estaba limpiando o crea una en el momento (una por clase en un frame normal, las que hagan falta
  en un resync). Esa es la emergencia: wgpu llena con ceros lo que no se limpió, y las
  estadísticas la cuentan.
- La tabla de chunks crece duplicando, por adelantado cuando quedan pocas slots libres o en el
  momento si el frame la necesita. La nueva empieza como copia en la GPU de las slots que se
  escribieron. Las slots no cambian nunca. La tabla nunca se achica: un pedido que la tabla ya
  pasó se descarta.
- Una creación de más de 2 ms (un bloque nuevo del driver) duplica la reserva de su clase una vez.
  Una que falla deja a su clase sin crecer 10 s.
- Un chunk rechazado guarda su malla (el mismo `Arc`, sin copia) y se reintenta, el más cercano
  primero, cuando aparece lugar. Cada vez es una sola pasada por todos, 131 072 quads por frame,
  y después nada hasta que vuelva a aparecer lugar. Uno rechazado por falta de slot no pide
  páginas mientras la tabla no pueda crecer.
- El techo se lee en cada frame. Bajarlo no saca nada residente.
- Con `grow: false` queda el piso fijo, más el reintento.

### 9. Encoger

Si una clase puede perder su página más vacía y aun así guardar su reserva (más lo que esperan sus
chunks rechazados) más media página, y sus lugares para chunks más los de media página,
durante 5 s y sin haber creado una página en los 10 s anteriores, esa página pasa a Draining. Ya
no recibe chunks. Sus chunks se copian de página a página a los huecos de las otras, unos MiB por
frame (32, 8 o 4, como la limpieza), y su entrada de la tabla cambia en el mismo frame. Vacía, se
suelta. Vuelve a Active si a la clase le faltan quads o lugares, o si en 2 s no pudo mover nada.
Es también la única desfragmentación.

Nunca baja del piso, contando las dos clases juntas. Solo se sueltan páginas del tamaño de
crecimiento: una del piso más chica no se podría volver a crear, y la memoria quedaría por encima
del piso.

### 10. Al cambiar de mundo

`clear_world` (ADR 0034) deja la memoria como la de un renderer nuevo: las páginas del piso, la
tabla y las listas iniciales, nada pedido ni en curso. Lo que enseñó el dispositivo se queda: las
esperas tras un fallo, la reserva duplicada y la creación más lenta.

## Mediciones (2026-10-13)

Detalle en [rendimiento/memoria-y-mundos-2026-10.md](../rendimiento/memoria-y-mundos-2026-10.md).

- La sonda decide 32 MiB. Dentro de un bloque, crear una página tarda microsegundos con los dos
  tamaños. Un bloque nuevo de 256 MiB tarda unos 25 ms en la 780M y de 2 a 13 ms en la RTX, igual
  con 32 que con 64 MiB.
- A radio 32 en la cueva, 9 páginas con chunks (18 multi-draws) no cuestan nada en la RTX y en la
  780M van un 7 % más rápido que el arena único.
- Crecer de radio 8 a 32 desde el piso: 0 chunks perdidos y 0 páginas de emergencia en las dos
  GPU.
- Encoger suelta una página cada 5 s como mucho: 2 de 9 a los 15 s y 8 a los 60 s. Queda una sobre
  el piso por la histéresis.

## Sin pisos fijos (2026-10-13)

La decisión 10 del plan pedía quitar los pisos fijos después de la ronda de control siguiente. La
ronda 2 no encontró empeoramientos
([rendimiento/control-2026-10-13.md](../rendimiento/control-2026-10-13.md)), así que se quitaron:

- `tools/perf` ya no arranca los casos de radio 32 con 48 Mi quads y 98 304 slots.
- Los ejemplos ya no le dan 24 Mi quads y 65 536 slots al overworld.

Todo arranca del piso por defecto (8 Mi quads, 32 768 slots) y crece. `--arena-quads`,
`--chunk-slots` y `--chunk-memory` siguen como opciones. Un binario de antes de las páginas (sin
`--chunk-memory`) sigue recibiendo los 48 Mi en radio 32, para que `perf-bisect` pueda medir a
través de #57. `snapshot` reserva todo antes de subir, como el primer frame del plugin.

El smoke en la RTX, radio 32 con tope 144, no ve diferencia: 142 y 143 fps en la superficie y en
la ruta de la cueva, igual que antes. Desde el piso, la superficie crea 4 páginas y la cueva 9, sin
chunks perdidos ni páginas de emergencia.

## Pendiente

- La decisión 7: el bloque nuevo pasa los 2 ms en la 780M (unos 25 ms, un frame largo cada 256 MiB
  de crecimiento). `MemoryHints::Manual` iría en `boxel_gfx`, que está protegido. Decide el dueño.
- Tras volver de radio 32 a 8, soltar las páginas de más tarda casi un minuto, no los 15 s del
  plan. Soltar de una vez las páginas que ya están vacías lo acortaría. Decide el dueño.

## Consecuencias

- Los tests de GPU comparan muchas páginas chicas contra una sola: la imagen es idéntica en cada
  tier, también lejos del origen.
- Entre chunks vecinos, Mid y Low pueden dibujar en otro orden que antes. Solo se nota en píxeles
  de borde con profundidad exactamente igual, dentro de la tolerancia entre tiers.
- La tabla de chunks y las páginas tienen `COPY_SRC` para moverse y leerse en tests.
- El proptest de `MemOp` también reproduce páginas nuevas, limpiezas, tablas que crecen y páginas
  que se vacían. Los tests de GPU comparan una memoria que crece y encoge contra una página grande,
  píxel a píxel en la escena sin vecinos.
- Mover un chunk puede cambiar su orden entre vecinos en Mid y Low, dentro de la misma tolerancia.
