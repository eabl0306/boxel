# ADR 0057: empujes de celda (columnas de burbujas)

- Estado: aceptado (pedido del dueño para el programa "todos los bloques"; amplía el ADR 0047,
  cuyo trabajo en el núcleo de `boxel_physics` el dueño aprobó)
- Fecha: 2026-10-31

## Contexto

La columna de burbujas de Minecraft (`BubbleColumnBlock`) es agua que empuja hacia arriba sobre
arena de almas y tira hacia abajo sobre magma. Por cada celda de la columna que toca la caja de
un cuerpo, cada tick, Minecraft cambia su velocidad vertical (`Entity.handleOnInsideBubbleColumn`):
hacia arriba `min(0.7, vy + 0.06)`, hacia abajo `max(-0.3, vy - 0.03)`. En la celda de arriba de
la columna, si encima no hay colisión ni fluido, empuja más (`handleOnAboveBubbleColumn`): hacia
arriba `min(1.8, vy + 0.1)`, hacia abajo `max(-0.9, vy - 0.03)`. Un cuerpo más rápido que el
tope vuelve a él. La caída se corta.

Las superficies del ADR 0047 no tienen nada así: arrastran, hacen trepar o resbalar, pero no
empujan. Y se ponían por bloque, cuando la columna empuja o tira según su propiedad `drag`.

## Decisión

- `Surface::push: Option<Push>`. Un `Push` tiene dos `Lift`, `inside` y `top`. Un `Lift` es una
  aceleración vertical (bloques por segundo al cuadrado, negativa hacia abajo) y un tope de
  velocidad con signo. En un paso de `dt`, `apply` deja `min(tope, v + a dt)` si empuja hacia
  arriba y `max(tope, v + a dt)` si tira hacia abajo, como Minecraft.
- Cuándo actúa: como el resto de las superficies de celda (ADR 0047), desde las celdas en que
  quedó la caja al final del paso anterior. `sense_inside` cuenta cada celda con empuje
  (`Inside::pushes`: hasta 4 `Lift` distintos, cada uno con cuántas celdas empujan con él, en el
  orden en que aparecen, de abajo hacia arriba). Una celda usa `top` si la de arriba no tiene
  colisión, no tiene fluido y no empuja; si no, `inside`. Esa celda de arriba se mira solo para
  las celdas con empuje; una que no está cargada cuenta como llena.
- En el paso, después de la gravedad y los arrastres, cada celda aplica su `Lift` una vez, en
  ese orden. Un personaje que vuela no es empujado.
- `Contacts::surface` dice `pushed` (una celda lo empujó: el juego corta la caída, como
  `resetFallDistance`) y `pulled` (una lo tiró hacia abajo). El nadador tirado hacia abajo no
  nada hacia arriba y su salto se gasta: en Minecraft la columna gana a nadar. `BodyCodec` guarda
  los dos bits nuevos; un cuerpo viejo se lee igual.
- `BlockSurfaces::set` acepta propiedades: `"game:bubble_column[drag=true]"` pone la superficie
  solo en los estados con esos valores, encima de la del bloque. Un nombre o una propiedad o un
  valor que el registro no tiene es un aviso.
- Determinismo: solo `+ - * /`, `min` y `max`. Igual lejos del origen y en el borde del mundo
  (tests a ±2^31, 2^40 y el límite).

## Cómo pasar los números de Minecraft

Un cambio de `d` bloques por tick en cada tick es `400 d` bloques por segundo al cuadrado; una
velocidad de `l` bloques por tick es `20 l` bloques por segundo. Hacia arriba: dentro 24 hasta
14, arriba 40 hasta 36. Hacia abajo: dentro -12 hasta -6, arriba -12 hasta -18.

Con el agua del motor (gravedad 8 y arrastre 3 por segundo, ADR 0021) un jugador en la columna
hacia arriba queda en 14 bloques por segundo con tres celdas en la caja y en 13,3 con dos
(Minecraft 14 y 11,5). Hacia abajo, en 6 (Minecraft 6).

## En el juego

- `bubble_column` es un bloque que siempre tiene agua (`waterlogged=[true]`, como el kelp): para
  los fluidos, el nado y el mesher es agua, y no dibuja nada más. Sin modelo, contorno ni
  colisión, reemplazable.
- Se forma y se va como `BubbleColumnBlock`, con las reacciones de los ticks (`TickHandler::react`)
  sobre los ticks de agua que ya tenían: una fuente de agua sobre arena de almas se vuelve
  columna hacia arriba, sobre magma columna hacia abajo, sobre una columna esa columna; una
  columna sobre otra cosa vuelve a fuente de agua. Así crece hacia arriba por las fuentes y se va
  desde abajo cuando cambia su base. Minecraft espera 20 ticks (la base) o 5 (la columna); aquí
  pasa en la misma escritura.
- Un cubo vacío la toma entera y deja aire; un bloque puesto en ella queda con agua.

## Medidas

- `tick/*` (criterion de `boxel_physics`, una corrida por lado, sin empujes en la tabla): 1.000
  cuerpos 531,8 a 516,0 µs, con superficies 573,6 a 568,2 µs, entre vallas 553,2 a 567,5 µs, sin
  cuerpos 1,57 a 1,64 µs. Ruido de una corrida: el paso suma una comparación.
- `just perf-smoke surface forest`: ok (la tabla está en el ADR 0056; el stress no tiene cuerpos).
- Tests: un caminante sube por una columna a lo sumo a 14 bloques por segundo mientras ninguna
  celda de su caja es la de arriba, sale por arriba y vuelve; uno que cae en una columna hacia
  abajo queda en 6 dentro y llega al piso; quien vuela no es empujado; un nadador que sube en una
  columna hacia abajo se hunde hasta el fondo; todo igual bit a bit a ±2^31, 2^40 y el límite del
  mundo.

## Lo que no se hizo

- Las partículas de burbujas: el juego no tiene partículas. Dibuja burbujas propias con quads de
  celda que se mueven (ADR 0054), cerca del ojo.
- El empuje de botes y objetos sueltos: no hay botes ni objetos en el mundo todavía. El motor ya
  empuja cualquier cuerpo dinámico.
- Las demoras de 20 y 5 ticks: la columna aparece y se va en el mismo tick.

## Consecuencias

- Una tabla sin empujes cuesta lo mismo: el paso mira un número más del `Inside`. Con empujes,
  cada celda con empuje que toca la caja lee la de arriba.
- `Inside` crece (cuatro `Lift` con su cuenta) y `Contacts::surface` usa dos bits más.
