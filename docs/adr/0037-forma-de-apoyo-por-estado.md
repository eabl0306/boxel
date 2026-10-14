# ADR 0037: forma de apoyo por estado

- Estado: aceptado. El dueño aprobó el item EW-SUPPORT (#112) el 2026-10-14 y tomó las dos
  recomendaciones del plan.
- Fecha: 2026-10-14

## Contexto

Hasta ahora las reglas preguntaban si una cara sostiene un bloque leyendo la colisión
(`rules::is_sturdy`, `rules::supports_center`). En Minecraft son dos formas distintas. El barro y
la arena de almas chocan a 14/16 de alto, para que uno se hunda un poco, pero sostienen una
antorcha como un cubo entero. La nieve de 8 capas choca a 14/16 y su apoyo es el cubo entero.
Con una sola forma, el juego tenía que elegir: o el jugador no se hunde, o la antorcha se cae.

## Decisión

- Una forma de apoyo por estado: `BlockRegistryBuilder::set_support(block, |estado| ...)`, que
  se lee con `BlockRegistry::support`. Es el `getBlockSupportShape` de Minecraft. Si la función
  da `None`, el estado usa su colisión, que es el valor por defecto. El contenido que nunca la
  llama se lee igual que antes.
- Fija todos los estados del bloque, así una llamada posterior reemplaza a la anterior, como en
  el ADR 0036. Las cajas se validan como cajas de colisión (hasta `MAX_COLLISION_HEIGHT`). Un
  estado inválido rechaza la llamada entera y no fija nada.
- `is_sturdy` y `supports_center` cambian de significado sin cambiar de nombre: leen el apoyo.
  Es lo que lee Minecraft (`isFaceSturdy`) y lo que quiere cada llamada de hoy.
  `collision_face_cover` sirve para cualquier forma.
- Qué lee cada cosa:
  - Apoyo: antorchas, antorchas de pared, vallas y muros que se conectan.
  - Colisión: la física, los fluidos (`canPassThroughWall` de Minecraft, el ADR 0025 sigue
    igual) y lo alto de un muro (el `WallBlock` de Minecraft corta la colisión del bloque de
    arriba).
  - Ni una ni otra: el mesher tapa caras por `StateFlags::OCCLUDES` y el contorno sale del
    modelo.
- Sin bits cacheados: las reglas no son un camino caliente. Una máscara por estado puede venir
  después detrás de la misma API.

## Consecuencias

- Nada cambia hasta que un juego fije un apoyo. El demo no fija ninguno, así sus tests y goldens
  siguen iguales.
- Cada regla nueva tiene que elegir entre apoyo y colisión. La lista de arriba dice cuál.
- El juego hace después su parte: barro, arena de almas y nieve chocan a 14/16 con apoyo entero.
  La regla de la nieve pasa a leer la colisión del bloque de abajo, y el pepino de mar la
  colisión o el apoyo, como en Minecraft.
