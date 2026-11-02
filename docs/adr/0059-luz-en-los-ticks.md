# ADR 0059: luz en los ticks

- Estado: aceptado (la parte que quedaba de la issue #84; el ADR 0041 la dejó afuera)
- Fecha: 2026-11-02

## Contexto

Los ticks aleatorios (ADR 0041) y programados (ADR 0021) leen bloques, pero no luz. En
Minecraft muchas reglas dependen de ella: los cultivos crecen con luz 9 o más, los brotes también
pero contando la hora del día, el pasto se extiende con luz, los hongos solo se extienden en la
oscuridad, el hielo y la nieve se derriten junto a una antorcha. Sin luz, el juego hacía crecer
todo a oscuras.

La luz del motor es una función pura de los bloques y de los techos de cielo (ADR 0015). Se
calcula dentro del job de malla y no se guarda en `ChunkData`. Lo único que queda para el
gameplay es la `ChunkLight` de cada chunk a menos de `light_cache_radius` (8) de un viewer, la
que lee `ChunkMap::light_at`. Los ticks aleatorios corren a 4 chunks, dentro de ese radio.

Minecraft tiene tres lecturas:

- `getBrightness(LightLayer.BLOCK, pos)`: solo la luz de bloque (hielo, nieve).
- `getRawBrightness(pos, 0)`: el máximo de cielo y bloque, sin la hora (cultivos, tallos,
  arbustos de bayas, bambú, hongos). Por eso un trigo bajo el cielo crece de noche.
- `getMaxLocalRawBrightness(pos)`: lo mismo con el cielo oscurecido por la hora del día,
  `skyDarken` (brotes, pasto). En 26.3 sale de una línea de tiempo de datos
  (`timeline/day.json`): 0 de día y 11 de noche.

## Decisión

- `TickContext::light(pos) -> Option<TickLight>`, con `TickLight { sky, block }`. El nivel de
  bloque es el canal más alto de rojo, verde y azul: Minecraft tiene un solo nivel.
- `TickLight::raw(darken)` es `getRawBrightness`: el cielo menos `darken` (sin bajar de 0) o el
  bloque, el mayor.
- `TickContext::brightness(pos)` es `getMaxLocalRawBrightness`: `raw` con
  `TickContext::sky_darken()`.
- `BlockTickSettings::sky_darken: fn(&TimeOfDay, &DaylightCurve) -> u8` da ese oscurecimiento.
  Se pide una vez por frame, en `First`: los ticks no leen el reloj en `Update`, donde las apps lo
  mueven (teclas, dormir), y no hace falta ordenarlos contra esos sistemas. Sin reloj
  (`DaylightPlugin`) vale 0. Por defecto,
  `curve_sky_darken` lo saca de la curva del motor: 0 de día, 6 con el sol en el horizonte y 12
  de noche. El juego pone el de Minecraft. El motor no nombra ningún bloque.
- **De dónde sale.** De la `ChunkLight` del chunk, la que calculó su último job de malla o de
  luz. Es una lectura de memoria: ni un cálculo de luz ni un lock. El `TickContext` guarda la
  luz del último chunk leído, y cada elección aleatoria empieza con la de su chunk (y con sus
  bloques).
- **Cuando la luz no está lista** (el chunk no está cargado, todavía no se malló ni se iluminó,
  o está a más de `light_cache_radius` de todo viewer): `None`. Un handler lo toma como una
  condición que no se cumple: el cultivo no crece en esa elección. El radio de los ticks
  aleatorios tiene que quedar dentro del de la caché de luz (4 y 8 por defecto).
- **Sin luz** (`WorldSettings::light` apagada): cielo abierto en todas partes
  (`TickLight::OPEN`), como lo ven sus mallas.
- Las escrituras de los ticks del frame no cambian la luz que leen los ticks siguientes.

## Determinismo

El valor leído es la luz exacta del contenido con que se calculó: no depende del orden ni de
los hilos. En un mundo quieto, sin una edición de luz esperando su remallado, la misma partida da
lo mismo.

Después de una edición que cambia la luz (una antorcha, un bloque que tapa el cielo), los ticks
leen la luz de antes hasta que llega el remallado del lote: 2 frames con tope 144 y de 3 a 5 sin
tope (issue #7, `docs/rendimiento/lotes-de-edicion-2026-11.md`). En qué frame llega depende de
los jobs, así que lo que pasa en ese tramo puede cambiar de una corrida a otra. Minecraft también
lee la luz atrasada: su motor de luz procesa los cambios después del `setBlock`.

Se descartó esperar la luz al día (saltear la elección si la luz del chunk no es la de sus
datos actuales). Cualquier edición del chunk, aunque no cambie la luz (un cultivo que crece),
la deja vieja hasta su remallado, así que un campo grande casi nunca tendría luz al día. Para
que la luz siga al día con ediciones que no la cambian hay que tocar el streaming (zona
protegida): queda para el dueño si hace falta.

## Medidas

- `cargo bench -p boxel_world --bench ticks`: un frame de un tick con 25.600 elecciones sobre
  plantas que leen el bloque de abajo. Sin leer luz, 648 µs; leyendo la luz y comparando con 9,
  800 µs. Unos 6 ns por lectura. Empezar cada elección con su chunk ya leído bajó la variante
  sin luz de 963 a 648 µs (-33 %).
- `just perf-smoke surface` (RTX, r16, sin tope): 2141 a 2147 fps (+0,3 %), p99 1,04 ms igual.
  Ok. Sus bloques no tienen handler, así que no corre ningún tick; la superficie varía unos
  ±10 % entre corridas con cualquier binario.
- En el juego, con el pasto, los brotes y el resto de las reglas de luz andando,
  `just perf-smoke surface`: 557 a 555 fps (-0,4 %), p99 4,37 a 4,44 ms. Ok.

## Fuera de esta parte

- Las reglas de vecinos (`NeighborRule`) no leen luz. En el juego, lo que Minecraft hace en
  `updateShape` con la luz (un cultivo que se rompe a oscuras, un hongo a pleno sol) va en el
  `react` de un `TickHandler`, que sí tiene `TickContext`.
- La lluvia, que en Minecraft también oscurece el cielo.
- La aparición de mobs, que espera a los mobs.
