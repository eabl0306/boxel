# ADR 0036: brillo pleno por estado

- Estado: aceptado. El dueño tomó las recomendaciones del plan de bloques el 2026-10-13 (item
  EW-GLOW, con el lote B2).
- Fecha: 2026-10-13

## Contexto

Desde el ADR 0015 sec. 12, todo bloque que emite luz se dibuja a brillo pleno: sin luz, sin
sombreado de cara y sin AO. Está bien para una lámpara, pero no para un emisor débil. Un bloque
que emite 3 brilla como una lámpara en una cueva oscura. En Minecraft el magma, la obsidiana
llorona o la mena de redstone emiten poca luz y se ven iluminadas por esa luz, no encendidas.

## Decisión

- Una marca por estado: `BlockRegistryBuilder::set_full_bright(block, |estado| ...)`. Se lee con
  `BlockRegistry::is_full_bright`. Marca o desmarca todos los estados del bloque, así una llamada
  posterior reemplaza a la anterior.
- Apagada por defecto, como en Minecraft. Un estado sin marca se ilumina con la luz que tiene
  alrededor, la suya incluida: sus caras muestran la luz del aire de enfrente (su emisión menos
  uno). Un estado que no emite también se puede marcar: la marca es un aspecto.
- Los fluidos no se marcan: brillan a pleno solo si emiten. En el shader el mismo bit elige
  también la velocidad del flujo de la lava y su animación. Iluminar la lava por su luz sería
  trabajo de shader y render.
- La lejanía (LOD) usa la misma marca, así no hay salto en el borde del LOD. Un emisor sin marca
  comparte su estado canónico con un bloque igual que no emite.
- `FLAG_EMISSIVE` conserva su nombre: es el bit 0 de los flags de `geo`, ahora "brillo pleno".
  `StateFlags::EMISSIVE` sigue diciendo "emite luz", porque lo usan las máscaras de luz.
- La marca no va en `BlockDef` ni en `StateFlags`: ningún literal se rompe y ningún golden
  cambia.
- Sin cambios de shader, render ni gfx: el mesher solo decide si pone un bit que los tres tiers
  ya leen.

## Consecuencias

- El demo marca sus lámparas y antorchas, así sus imágenes no cambian (los goldens de malla y de
  contenido siguen iguales). El conjunto `test:` marca `test:lamp`.
- Cada juego marca sus propios bloques. Un emisor del juego que no marque se ve iluminado.
- Las capas `_emissive` de los packs (un brillo por textura, como en `open_eyeblossom`) siguen
  fuera: hacen falta un segundo quad o un cambio de shader.
