# ADR 0033: la interfaz del motor (`boxel_ui`)

- Estado: aceptado. El dueño eligió la interfaz propia el 2026-10-12 (issue #39, plan de menús).
  Primera parte, el crate (E-UI1), y segunda, el pase de GPU (E-UI2, sección 6).
- Fecha: 2026-10-12

## Contexto

El juego necesita menús (título, pausa, opciones, muerte, mundos) y un HUD. La issue #39 pedía
decidir entre una interfaz inmediata o retenida, que funcione en los tres tiers y sin un costo
alto por frame. Había dos caminos: egui con un pintor nuestro, o una interfaz propia.

## Decisión

### 1. Una interfaz propia y opcional

Un crate nuevo, `boxel_ui`, sin dependencias externas. La fachada lo expone con la feature `ui`,
apagada por defecto, y `UiPlugin` no está en `DefaultPlugins`. Un juego que no lo agrega no paga
nada.

No es egui porque:

- egui suma unos 20 crates, builds más largos en el portátil y una versión que rompe cada pocas
  semanas.
- Los packs traen sprites de GUI con nine-slice y fuentes de mapa de bits. Eso lo resolvemos
  directo; egui no lo tiene y igual necesitaría widgets propios.
- Con nuestra regla de dibujo los tres tiers dan los mismos píxeles por construcción.

### 2. El motor retiene y el juego declara

- El juego declara: cada pantalla es un sistema común que toma `Ui`, abre una capa y declara sus
  widgets en cada frame que está abierta (`p.button(rect, "Jugar").clicked`). Tamaños fijos en
  píxeles de GUI (`Column`, `Grid2`, `UiRect::split`), sin solver de layout y sin una entidad
  por widget.
- El motor retiene: el estado de los widgets (foco, el presionado, el arrastrado), una lista de
  quads por capa (`Hud`, `Screen`, `Popup`) y la lista compuesta `UiDraw`, cuya generación solo
  cambia cuando cambian sus bytes. El HUD guarda sus quads hasta que el juego lo redibuja: un HUD
  quieto y un menú sin cambios no suben nada.
- Hover y clics usan los rectángulos del frame anterior (un frame de atraso, no se nota porque
  las pantallas no se mueven), recortados igual que su dibujo. Lo que no se ve no recibe
  entrada: ni la parte recortada de un widget ni lo que queda bajo una capa modal.
- `InputCapture` dice si la interfaz toma el puntero y el teclado. El juego frena con eso su
  propia entrada (picking, jugador, hotbar). Escape es siempre del juego.

### 3. Una textura y aritmética entera

- Todo sale de una página `UiAtlas` RGBA8 sRGB. El texel (0, 0) es blanco opaco, así que un
  rectángulo sólido es un quad más.
- El texel de cada píxel es `src + (p - origen) * tamaño_src / tamaño` en enteros
  (`UiQuad::texel_at`). Sin sampler ni filtrado. El tinte multiplica en luz lineal y se mezcla
  con alfa normal.
- `raster` es la referencia en CPU de ese dibujo. Los tests comparan contra ella, también los del
  pase de GPU en High, Mid y Low (sección 6).
- La escala de GUI es entera, como el "GUI Scale" de Minecraft: la mayor que deja al menos
  320x240 píxeles de GUI, o la que elige el jugador si entra.

### 4. Sin contenido en el motor

- El aspecto por defecto es procedural (`UiSkin::procedural`, dibujado con código).
- La fuente por defecto es `font8x8`, de dominio público, con ASCII y Latin-1 (á, é, ñ, ¿, ¡).
- El juego mapea sus sprites a los slots del skin y arma su fuente con `FontBuilder` (una grilla
  como el proveedor de Minecraft, glifos derivados y acentos compuestos). Rutas de packs, textos
  en español y nombres del juego viven en el juego. `tools/no_content.py` revisa el crate.

### 5. Sin puente por el overlay F3

El plan proponía pintar la interfaz con el overlay F3 hasta tener el pase de GPU. El dueño aprobó
el pase (E-UI2) y llega antes de los menús, así que no hay puente.

### 6. El pase de GPU (E-UI2)

- `UiPass` vive en `boxel_render` detrás de su feature `ui`. La fachada la prende con `ui` y
  `render`. Dibuja `UiDraw` justo después del overlay y antes del blit: las capturas incluyen la
  interfaz y los menús tapan el F3 y la mira.
- Costo cero sin uso: el pase se crea con la primera lista que no está vacía y no abre nada
  mientras la lista está vacía. `Extract` copia la lista y el atlas solo cuando el pase no tiene
  su generación, y se suben solo cuando cambian (o cambia el tamaño del frame).
- Liga una sola textura y la lee con `textureLoad`, sin sampler, uniform ni immediates
  (`ui.wgsl`). Corre en Low y en GL 3.3.
- El mapeo de cada quad viaja en enteros relativos a su primer píxel dibujado, no a su origen:
  el texel es exacto aunque el origen esté muy fuera de la pantalla. La instancia ocupa 64 bytes
  (el plan decía 48 con el origen en 16 bits).
- Lo opaco sale igual a `raster`, byte a byte, en High, Mid y Low (también Low en GL). Cada
  mezcla translúcida puede redondear un paso distinto, porque el mezclador de la GPU no usa
  nuestras tablas: un píxel puede quedar tantos pasos lejos como capas translúcidas tiene encima
  (`tests/gpu_ui.rs`). Los tiers de una misma GPU dan los mismos bytes.

## Consecuencias

- `boxel_window` suma `CursorPosition`: la última posición del cursor en píxeles físicos.
- La lista de pintura y la entrada funcionan sin ventana, y `UiTester` hace clic por etiqueta en
  los tests. Con `render` y la feature `ui`, la interfaz se ve en pantalla.
- Lo que sigue: E-UI3 (lista, campo de texto y pestañas), E-UI4 (texto con acentos desde el
  runner, protegido) y E-UI5 (sprites y fuentes de los packs).
- Las capas `Screen` y `Popup` se vacían en cada frame que nadie las dibuja, así que una pantalla
  cerrada desaparece sola. El HUD se vacía cuando cambia la escala o el tamaño de la ventana: el
  juego lo redibuja con `resource_changed::<GuiScale>`.
- Los paneles translúcidos se ven un poco más claros que en Minecraft, porque mezclamos en luz
  lineal. Su alfa se ajusta a ojo.
