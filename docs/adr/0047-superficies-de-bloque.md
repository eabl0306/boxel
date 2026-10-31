# ADR 0047: superficies de bloque en la física

- Estado: aceptado (el dueño aprobó el trabajo en el núcleo de `boxel_physics` para el programa
  "todos los bloques", 2026-10-31; es el punto "fricción por bloque" de la issue #17)
- Fecha: 2026-10-31

## Contexto

La fricción era de cada cuerpo (`BodySettings`). Un juego tipo Minecraft necesita que el bloque
cambie cómo se mueve el cuerpo: el hielo resbala, la arena de almas frena, la miel frena y salta
poco, la telaraña atrapa, la escalera se trepa, el andamio se trepa y solo sostiene desde
arriba, la nieve polvo se hunde salvo con botas de cuero y por el costado de la miel se resbala
despacio.

## Decisión

- `Surface` en el núcleo puro (`core::surface`), por bloque y con datos que pone el juego. El
  motor no nombra contenido. Campos, todos en unidades del motor (bloques, segundos, tasas por
  segundo):
  - `friction`: se suma a la fricción de suelo del cuerpo parado encima (negativa: resbala). El
    controlador la compensa: en hielo acelera más lento y llega a la misma velocidad.
  - `drag`: arrastre horizontal extra encima. El controlador no lo compensa: se camina más lento.
  - `jump`: multiplica la velocidad del salto desde el bloque.
  - `bounce`: la restitución de un aterrizaje encima. Agachado no rebota.
  - `inside`: arrastre extra (horizontal y vertical) mientras la caja del cuerpo toca la celda.
  - `climb`: un personaje con los pies en la celda trepa (`Climb`: velocidad de subida, tope de
    velocidad, si agacharse lo sostiene y qué `Treads` pide).
  - `slide`: un cuerpo que cae pegado a un costado del bloque, debajo de su tope, baja a esa
    velocidad.
  - `hold`: la colisión solo sostiene cuerpos que empiezan el movimiento por encima de ella (con
    1e-5 de tolerancia, como `isAbove`), que no bajan (agachados) y que tienen los `Treads` que
    pide. Rayos y consultas ven la colisión registrada.
- El juego pone `BlockSurfaces` (nombre de bloque y superficie). `PhysicsPlugin` la vuelca en
  `CollisionTable` antes del paso siguiente, para todos los estados del bloque. Un nombre que el
  registro no tiene es un aviso.
- `Treads` es el gancho del juego: bits en el cuerpo que algunas superficies piden. El juego los
  pone según lo que lleva puesto (botas de cuero para la nieve polvo).
- Cuándo actúa cada cosa, como en Minecraft (el efecto de un tick se aplica en el siguiente):
  - Suelo: el bloque de `Contacts::ground_block` del paso anterior. El controlador y el paso lo
    leen (una lectura de bloque por cuerpo apoyado, solo si alguna superficie actúa en el suelo).
  - Celda: el barrido marca al final si la caja quedó en una celda con efecto
    (`SurfaceContacts::near`, gratis: es la misma pasada que busca agua). Solo entonces el paso
    siguiente mira esas celdas (`sense_inside`). Gana el arrastre más fuerte. La trepada mira la
    celda de los pies.
  - Barrido: `move_box_treading` recibe el `Tread` del cuerpo (agachado y sus treads). Un bloque
    con `hold` que no lo sostiene se lee como vacío. La guarda de borde al agacharse no cuenta
    esos bloques como apoyo.
- `Contacts::surface` dice qué hicieron las superficies en el paso (trepó, atrapado, resbaló) para
  que el juego corte la caída, como Minecraft. `BodyCodec` guarda esos bits y los `Treads` (bit 15
  de las banderas): un cuerpo guardado sigue igual bit a bit. Los cuerpos viejos se leen igual.
- Determinismo: solo `+ - * /`, `min`, `max`, `clamp`. Todo en el marco del bloque, igual lejos
  del origen y en el borde del mundo (tests a ±2^31, 2^40 y el límite).

## Cómo pasar los números de Minecraft

Minecraft corre a 20 ticks por segundo y multiplica la velocidad por factores en cada tick. El
motor usa tasas por segundo, así que se convierte una vez, en el contenido, con estas fórmulas
(números del jar 26.3; el jugador del motor tiene fricción de suelo 12, arrastre vertical 0.4 y
gravedad 32):

- Fricción `f` del bloque: el cuerpo conserva `f * 0.91` por tick y la tasa es `-20 ln(conserva)`.
  Respecto del bloque normal (0.6) es `friction = 20 ln(0.6 / f)`. Hielo, hielo compacto y hielo
  escarchado (0.98): -9.81. Hielo azul (0.989): -10.0. Slime (0.8): -5.75.
- Factor de velocidad `s` (arena de almas y miel, 0.4): a paso constante el jugador queda en
  `(1 - 0.546) / (1 - 0.546 s)` = 58 % de su velocidad. Con el arrastre `drag = 12 (1 / 0.58 - 1)`
  = 8.66 el controlador llega a lo mismo.
- Factor de salto (miel, 0.5): `jump = 0.5`, igual.
- Multiplicador de celda `m` (telaraña 0.25 y 0.05, arbusto de bayas 0.8 y 0.75, nieve polvo 0.9 y
  1.5): Minecraft multiplica el movimiento del tick y pone la velocidad en cero, así que la
  velocidad queda en `m` veces lo que se gana en un tick desde cero. Horizontal:
  `across = 12 (1 / (0.454 m) - 1)` (93.73, 21.04 y 17.37). Vertical:
  `vertical = 32 / (1.568 m) - 0.4` (407.76, 26.81 y 13.21). La nieve polvo hunde a 2.35 bloques/s,
  como en Minecraft.
- Trepar (escalera, enredaderas, andamio): Minecraft pone 0.2 por tick y después aplica gravedad
  y arrastre: sube a `(0.2 - 0.08) * 0.98 * 20` = 2.352 bloques/s. Tope de 0.15 por tick: 3
  bloques/s. Con botas, la nieve polvo se trepa a 2.352 * 1.5 = 3.528, sin tope.
- Miel por el costado: baja a `(0.05 + 0.08) * 0.98 * 20` = 2.548 bloques/s. Empieza si cae a más
  de 1.6 (0.08 por tick). Si entra cayendo a más de 2.6 (0.13 por tick), la velocidad horizontal
  se multiplica por `1 / caída`.

## Lo que se aproxima

- Velocidades a paso constante iguales a Minecraft. Lo transitorio cambia un poco: en hielo el
  tope es 4.317 (Minecraft 4.16) y la aceleración es 19 % de la normal (Minecraft 23 %).
- Arena de almas y miel frenan por arrastre mientras el cuerpo está apoyado. Minecraft también lo
  aplica medio bloque en el aire.
- La nieve polvo atrapa a quien toca la celda. Minecraft atrapa a los seres vivos solo con los
  pies dentro.
- Caer de más de 2.5 bloques sobre nieve polvo: Minecraft aterriza un tick en una caja de 0.9 y
  no hace daño. Aquí el cuerpo entra y queda atrapado: la caída se corta igual.
- Andamio colgado (`bottom`, con distancia): Minecraft le da un piso fino desde dentro. Aquí no.
- Una trampilla abierta sobre una escalera no se trepa.

## Lo que no se hizo

- El slime y la columna de burbujas: son contenido excluido o piden EW-WATER. El rebote sí está
  en el motor, por si hace falta.
- El daño del arbusto de bayas y el frío de la nieve polvo: son reglas del juego.

## Consecuencias

- Una tabla sin superficies cuesta lo mismo: el barrido mira un bit más y nada más. Con
  superficies, cada cuerpo apoyado lee su bloque y los que tocan una celda con efecto la miran
  (`tick/1000_surfaces`).
- `move_body` recibe el `Tread`. `Contacts` tiene `surface`.
