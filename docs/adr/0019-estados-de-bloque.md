# ADR 0019: estados de bloque

- Estado: aceptado (segunda parte de la issue #38)
- Fecha: 2026-10-07
- Revisa: ADR 0018 (las variantes de tronco y de losa)

## Contexto

Con el ADR 0018 cada variante de un bloque era un bloque aparte: `boxel:log`, `boxel:log_x`,
`boxel:log_z`, `boxel:stone_slab` y `boxel:stone_slab_top`. Funciona para dos o tres variantes,
pero lo que viene no entra así: una escalera tiene 40 combinaciones de `facing`, `half` y
`shape` (80 con `waterlogged`), una cerca 16 de sus cuatro lados, el agua 16 niveles. Escribir un
`BlockDef` por variante no escala, el nombre no dice qué variante es y poner un bloque obliga a
armar nombres con sufijos (`_top`, `_x`).

Además, los resource packs de Minecraft eligen el modelo de un bloque por su texto de propiedades
(`axis=y`, `type=top`). Si nuestros estados tienen las mismas propiedades, el pack se lee tal cual.

## Decisión

### 1. Modelo: el "flattening" de Minecraft

- Un bloque tiene propiedades con nombre, cada una con una lista finita de valores en texto
  (`Property`: `axis` = `x | y | z`, `type` = `bottom | top | double`). Hay atajos para booleanos
  (`Property::boolean`, `false | true`) y enteros (`Property::int("level", 0..=15)`). Cada
  propiedad tiene un valor por defecto (el primero, o el de `with_default`).
- Cada combinación de valores es un `BlockState(u16)`. Los estados de un bloque tienen ids
  seguidos, en orden de registro. El estado por defecto tiene cada propiedad en su valor por
  defecto.
- Las propiedades se guardan ordenadas por nombre, como en Minecraft. Así el texto de
  propiedades (`facing=east,half=bottom,shape=straight`) es exactamente la clave de variante de un
  blockstate de Minecraft.
- El id dentro del bloque es mixto: la última propiedad cambia más rápido (paso 1) y el paso de
  cada una es el producto de la cantidad de valores de las que vienen después. El valor de una
  propiedad sale de `(id - primero) / paso % cantidad`, sin tablas extra por estado.

### 2. API del registro

- `by_name("boxel:log")` sigue igual y da el estado por defecto (`boxel:log[axis=y]`).
- `parse_state("boxel:stone_slab[type=top]")` lee un estado. Las propiedades que faltan toman su
  valor por defecto. Un bloque, propiedad o valor desconocido, una propiedad repetida o un texto
  mal formado son errores (`StateParseError`). `state_name(s)` lo escribe (`Display`), siempre con
  todas las propiedades, y es el inverso exacto.
- `property(s, "axis")` da el valor, `with(s, "axis", "x")` cambia uno y deja los demás,
  `values(s)` los recorre y su `Display` es la clave de variante (`axis=x`).
- Bloques: `BlockId` es el índice del bloque. `block(s)`, `block_by_name`, `block_name`,
  `default_state`, `states(b)` (los ids del bloque, en orden), `properties(b)`, `blocks()` y
  `block_count()`.
- `name(s)` sigue dando el nombre del bloque, igual para todos sus estados.

### 3. Registro: un `BlockDef` y una función por estado

- `BlockDef` no cambia: sigue siendo los datos de un estado. Los literales de `BlockDef` que ya
  había compilan sin tocarlos y `add_block` registra un bloque sin propiedades, como antes.
- `add_block_with_states(def, properties, f)` registra un bloque con propiedades. Cada estado
  arranca de una copia de `def` y `f(valores, &mut def)` la ajusta: modelo, colisión, opacidad,
  texturas. Una escalera de 80 estados es una función, no 80 definiciones. Cada estado pasa los
  mismos controles que un bloque suelto (luz, cajas de colisión, texturas) y conserva el nombre.
  La función se evalúa al registrar y no se guarda: el registro sigue siendo datos planos.
- Se rechazan propiedades con nombres o valores fuera de `[a-z0-9_]`, sin valores, con valores
  repetidos, con un valor por defecto que no está en la lista o con el mismo nombre dos veces
  (`RegistryError::InvalidProperty`). Un bloque que falla no deja estados a medias.

### 4. Datos por estado, igual de rápidos

- Todos los datos por estado siguen en arreglos planos indexados por el id: capa de dibujo,
  modelo, colisión, flags, emisión, opacidad de luz y texturas. Se suma uno solo, el bloque de
  cada estado (`u16`), que no usa ningún camino caliente.
- El material del mesher sigue siendo el id del estado. `MaterialTable`, `LightTables` y
  `CollisionTable` se arman igual que antes, recorriendo los estados. El mesher, la luz y la
  física no cambian: no hubo que medir de nuevo.
- Límite: `MAX_STATES` = 2^15 = 32 768 estados. El bit alto del material marca la superficie del
  agua (`FLUID_SURFACE_BIT`, ADR 0018), así que todo id debe quedar debajo. Un bloque que pasaría
  el límite da `RegistryError::Full`. Minecraft vanilla tiene menos de 30 000 estados, así que un
  clon entra, aunque sin mucho margen. Si algún día no alcanza, la marca de superficie tiene que
  salir del material (por ejemplo a una máscara aparte en el gather).

### 5. Contenido migrado

| Antes | Ahora |
| --- | --- |
| `boxel:log`, `boxel:log_x`, `boxel:log_z` | `boxel:log[axis=x\|y\|z]`, por defecto `y` |
| `boxel:stone_slab`, `boxel:stone_slab_top` | `boxel:stone_slab[type=bottom\|top\|double]`, por defecto `bottom` |

- `type=double` es un cubo opaco con las texturas de piedra: colisión completa, opacidad de luz
  15, tapa caras vecinas y el greedy lo fusiona como a cualquier cubo. Para el mesher es igual a
  la piedra (mismas capas, mismos quads).
- Ids del contenido de serie, congelados de nuevo: 0 aire, 1 a 7 piedra a bedrock, 8 a 10 el
  tronco (`x`, `y`, `z`), 11 hojas, 12 agua, 13 tablones, 14 a 17 lámparas, 18 lava, 19 vidrio,
  20 a 22 la losa (`bottom`, `top`, `double`), 23 pasto corto y 24 amapola. Son 21 bloques y 25
  estados. Los ids no se guardan en disco, así que el cambio solo afecta tests.
- El generador de árboles sigue pidiendo `boxel:log`, que ahora es `axis=y`.

### 6. Poner bloques (`voxel_world`)

- La regla sale de las propiedades, no de sufijos en el nombre: un bloque con `type`
  `bottom | top | double` es una losa (arriba o abajo según el clic, como en el ADR 0018) y uno
  con `axis` se acuesta en el eje de la cara clickeada.
- Losa doble, como en Minecraft: un clic con una losa en la cara de adentro de una losa del mismo
  bloque (el techo de una de abajo, el piso de una de arriba) llena su celda. También se llena la
  celda de enfrente si tiene una losa del mismo bloque y la nueva sería la otra mitad. El chequeo
  contra cuerpos usa la forma del cubo entero. La regla vive en `slab_merge`, con test propio.
- F3 muestra el estado completo del bloque apuntado (`boxel:log[axis=x]`).
- `examples/snapshot` acepta estados en `--place`, `--drop` y `--fill`
  (`stone_slab[type=top]@X,Y,Z`).

### 7. Resource packs

- Cada receta de textura nombra, si hace falta, un estado nuestro (`boxel:log[axis=y]`). Sus
  valores, más los de propiedades que solo tiene Minecraft (`snowy=false`), eligen la variante
  del blockstate del pack: la clave cuyas condiciones se cumplen todas y nombra más propiedades
  (`""` se cumple siempre), o la primera si ninguna se cumple. En un `multipart`, el primer caso
  cuya condición se cumple.
- Así cada uno de nuestros estados se lee del pack con su propio texto de propiedades: los tres
  tipos de losa caen en `type=bottom`, `type=top` y `type=double` de `stone_slab`, y los tres
  troncos en `axis=x|y|z` de `oak_log`. Las texturas siguen siendo por capa: la rotación de la
  variante se ignora, porque el tronco gira su textura solo.

### 8. Lo que viene (no está hecho)

El diseño ya cubre la segunda ronda sin cambios de modelo:

- Escaleras: `facing` (n, s, e, o), `half` y `shape` (5 valores). Los datos por estado salen de
  una función de esos valores. Falta el modelo de escalera en el mesher y sus cajas de colisión.
- Cercas: `north`, `east`, `south`, `west` booleanos. Al poner un bloque o cambiar un vecino se
  recalculan con `with`. Falta el sistema que actualiza vecinos.
- Antorchas de pared: `facing`. Agua: `level` 0 a 15, con altura por esquina cuando fluya.

## Consecuencias

- Agregar una variante es agregar un valor a una propiedad, no un bloque nuevo.
- El nombre de un estado se puede guardar y leer (`state_name` y `parse_state`): es lo que va a
  usar el IdMap de persistencia (M1) en lugar de ids crudos.
- Los ids del contenido de serie cambiaron (por ejemplo las hojas pasaron de 9 a 11). Los tests
  que los usaban se actualizaron; el código usa nombres.
- Un bloque con muchas propiedades gasta ids rápido (un producto). El límite de 2^15 lo frena con
  un error claro al registrar.

## Alternativas descartadas

- Agregar un campo `properties` a `BlockDef`: obliga a tocar cada literal y no resuelve los datos
  por estado. La función por estado lo hace sin tocar nada.
- Guardar los valores de cada estado en una tabla: ocupa memoria por estado. Los pasos por
  propiedad alcanzan para calcularlos.
- Mantener las propiedades en el orden en que se declaran: el texto no coincidiría con las claves
  de Minecraft, que van ordenadas por nombre.
- Subir el límite a 65 535 estados: el mesher necesita el bit alto del material para la
  superficie del agua.
