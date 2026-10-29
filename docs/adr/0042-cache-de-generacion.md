# ADR 0042: caché de generación

- Estado: aceptado (el dueño aprobó el estudio `.claude/plans/chunk-cache-study.md`).
- Fecha: 2026-10-29
- Revisa: ADR 0024 (la alternativa descartada "guardar todo chunk que se cargó" y la sección 5) y
  la decisión 8 de ADR 0031 ("nada del LOD generado se guarda en disco"). Usa las versiones de
  ADR 0039 (secciones 11 y 17).

## Contexto

Con TFC, volver a un mundo ya visitado tarda casi lo mismo que la primera vez: en el spawn a r16
con el LOD `far` el mundo se asentaba en unos 48 s, 35 de ellos llenando el LOD lejano. Todo eso
sale del generador otra vez, aunque el resultado es el mismo. El estudio midió que leer un chunk
cuesta unos 0,05 ms y generarlo 1,5 ms, y que la fuente del LOD del spawn son 77 s de CPU en solo
13,6 MB.

ADR 0024 decidió guardar solo lo editado y ADR 0031 no guardar nada del LOD. Las dos cosas
siguen valiendo para el mundo. Lo nuevo es una caché aparte.

## Decisión

1. **Una caché, nunca el mundo.** Un archivo propio, `cache.redb`, junto a `world.redb`
   (`persist::GenCache`, `CACHE_FILE`). Borrarlo no pierde nada: el mundo sigue siendo semilla,
   generador y ediciones. No usa la tabla `chunks`: el LOD trata todo chunk guardado como
   editado y la caminata de la luz empieza en el chunk guardado más alto.
2. **Las ediciones mandan.** El job de un chunk lee primero la copia guardada (`world.redb` o la
   copia pendiente); la caché solo se consulta para un chunk que nadie guardó. Un chunk editado
   nunca entra a la caché: la caché guarda solo lo que devolvió `generate`.
3. **Qué guarda.**
   - Chunks generados, exactamente como los devolvió el generador: el almacenamiento sin
     compactar y su `version`. Un acierto es el mismo `ChunkData` que dar la generación, así la
     luz, las mallas, el agua y los ticks aleatorios salen iguales. El aire uniforme no se guarda
     (generarlo es casi gratis). La piedra uniforme va como un registro de unos bytes.
   - El LOD lejano: por columna de nodos, la banda (`LodColumn`) con las capas que la fuente
     emitió, en su orden, y cada nodo (uniforme, o sus celdas con paleta). Nunca mallas ni luz.
     Las pirámides de las ediciones se ponen encima como siempre. Una columna cuyo segundo job
     pide menos capas suma sus capas a la banda guardada.
4. **Sellos.** Cada registro lleva un sello: nombre y versión del generador (o de su fuente de
   LOD), un hash de los nombres de los estados del registro en orden de id, y la sal del juego. Los
   bloques se guardan por su id de ejecución, que el hash del registro fija. Otro sello, un
   registro dañado o uno que falta es un fallo: se genera y se vuelve a escribir.
5. **La sal es del juego.** boxelcraft mezcla la semilla, una versión de su política, la versión
   del juego y la identidad del ejecutable (tamaño y fecha). Así un mundo nuevo en una carpeta vieja
   no lee lo del viejo, y una compilación nueva no lee terreno de otra aunque una etapa haya
   cambiado su salida sin subir su versión. Además, un mundo nuevo borra el `cache.redb` que haya
   en su carpeta.
6. **Escrituras sin frenar el frame.** El job que generó codifica el registro y lo ofrece a una
   cola acotada (1.024 registros). Si la cola está llena, el registro se tira. Un hilo propio por
   caché escribe por lotes de hasta 512, sin fsync (durabilidad `None` de redb), y hace durable lo
   escrito cada 5 s, en `flush` y al cerrar. Nunca pasa por el saver del mundo ni por su "un solo
   momento en el disco".
7. **Tope.** Los registros se agrupan por región (32 por 32 columnas de chunks, la clave empieza
   por la región). El escritor lleva el tamaño estimado en disco de cada región y cuándo se usó.
   Pasado el tope (1 GiB por mundo en el juego) borra regiones enteras: primero las de un sello
   que esta corrida no usó, después las usadas hace más tiempo, hasta el 90 % del tope.
8. **Prendida y apagada.** `GenCache::set_enabled` la apaga en caliente. En el juego: "Caché de
   terreno" en Opciones, Avanzado (prendida por defecto), y "Vaciar caché", que vacía la del mundo
   abierto (y compacta su archivo) y borra la de los demás mundos.
9. **Fallas.** Un archivo que no es una caché de esta versión se vacía. Uno que tiene otro proceso
   (dos juegos abiertos) no abre y el mundo juega sin caché. Una lectura o escritura que falla es
   un fallo de caché y se cuenta.

La línea `cache` del `DiagnosticsBoard` (F3) dice aciertos, fallos, escritos, tirados, dañados,
regiones desalojadas y el tamaño.

## Números medidos

Caso `surface` del juego (TFC, r16, LOD `far`, spawn de la semilla de los casos), Radeon 780M
(la NVIDIA no respondía), máquina quieta, dos corridas de cada uno. "Se asienta" es el
`settle_s` del caso: el mundo y el LOD lejano quietos.

| | se asienta (s) |
|---|---|
| sin caché | 37,0 y 37,6 |
| con caché, primera visita (vacía) | 38,7 y 38,3 |
| con caché, segunda visita | 3,65 y 3,92 |

- La segunda visita lee 11.515 de los 22.862 chunks con job (el resto es aire uniforme, que no se
  guarda) y todas las columnas lejanas (unas 8.950), sin escribir nada.
- La primera visita tarda 1 s más (de 2 a 4 %): codificar en los hilos de generación y la cola.
  No se tiró ningún registro.
- Disco (`du`): 59 MB después de asentarse en el spawn, 82 MB después de los 10 s de vuelo del
  caso (unos 640 bloques). La estimación del escritor dio 63 y 85 MB.
- Frames (`just perf-smoke --gpu 780m surface`, contra `main`; el mundo del caso no se guarda,
  así que mide el camino sin caché): 189 a 189 fps, p99 11,20 a 11,47 ms, "ok". Con la caché, en
  las corridas de arriba, la primera visita dio 186,6 fps y la segunda 190,7.

La segunda visita se mide con `--terrain-cache DIR` del juego (oculto): el mundo del caso queda en
memoria y su caché en DIR.

## Consecuencias

- La segunda visita se asienta unas diez veces antes; la primera paga la codificación y nada
  más (un 2 a 4 % del tiempo hasta asentarse).
- El disco crece con lo explorado hasta el tope. redb no achica el archivo: reusa sus páginas
  libres. "Vaciar caché" lo compacta.
- Una etapa que cambia su salida sin subir su versión ya era un error; con la caché se vería. La
  sal con la identidad del ejecutable lo evita a costa de vaciar la caché en cada compilación.
  Cuando las versiones de las etapas sean de fiar, el juego puede sacar el ejecutable de la sal.
- Un estado que ya no existe en el registro cambia el hash: toda la caché falla y se reescribe.
- La caminata de luz de una columna todavía genera los chunks que no están cargados. Que lea la
  caché es una mejora aparte, con su test de igualdad.

## Alternativas descartadas

- **Guardar lo generado en `world.redb`, como Minecraft.** El mundo pasaría a ser lo generado: no
  se podría desalojar, y un cambio del generador dejaría costuras en el borde de lo explorado.
- **Tirar toda la caché al abrir con otra versión.** Volver a una versión anterior lo perdería
  todo. Con el sello por registro solo fallan los de otra versión, y se desalojan primero.
- **Marcas por columna para la piedra uniforme.** Ahorran unos 0,6 MB por zona r16 contra 28 MB.
  No vale la complejidad todavía.
- **Guardar las mallas del LOD.** Pesan 4,5 veces más que las celdas y rehacerlas es barato.

## Cómo se probó

En `crates/boxel_world`:

- `persist/cache/tests.rs`: un chunk vuelve igual a como lo dejó el generador (paleta sin
  compactar, directo, uniforme, su versión), también lejos del origen; el aire uniforme no se
  guarda; otro sello (sal, parte, registro) falla; cada byte dañado de un registro es un fallo o
  el mismo chunk, nunca otro; una columna del LOD vuelve en el orden de la fuente en todos los
  niveles y lejos del origen; una columna con un nodo que falta falla hasta que está completa, y
  las bandas se suman; celdas al azar vuelven iguales (proptest); el tope borra la región más vieja
  y las de otro sello primero; vaciar; una cola llena tira registros; apagada y cerrada.
- `lod/tests_cache.rs`: un trabajo de columna con la caché (fallo y acierto) arma lo mismo que sin
  ella, con ediciones encima, en los niveles 1 a 4 y lejos del origen; un mundo que vuelve con la
  caché llena dibuja los mismos nodos con las mismas mallas sin llamar a la fuente.
- `tests/gen_cache.rs`: un mundo que vuelve con la caché llena tiene los mismos chunks, luz,
  techos de cielo y mallas que sin ella, cerca del origen, en ±2^31, en 2^40 y junto al límite; el
  agua corre igual y los ticks aleatorios caen igual; otra sal regenera; una edición gana a la
  caché y no entra en ella; un archivo dañado empieza vacío; un mundo viejo sin caché abre igual;
  una caché que tiene otro proceso se rechaza; apagada no lee ni escribe; las columnas lejanas de
  terrenos de los ejemplos vuelven iguales, en su orden, en los niveles 1 a 4 y en 2^40.

En el juego: los tests fijados `the_pinned_chunks_keep_their_blocks` y
`the_pinned_far_nodes_keep_their_cells` siguen iguales, y dos tests nuevos leen esos mismos chunks
y nodos de TFC a través de la caché y dan los mismos dos digests, con el orden de la fuente.
