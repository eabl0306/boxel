# ADR 0033: la interfaz del motor (`boxel_ui`)

- Estado: aceptado. El dueño eligió la interfaz propia el 2026-10-12 (issue #39, plan de menús).
  Partes: el crate (E-UI1), el pase de GPU (E-UI2, sección 6), texto e IME (E-UI4, sección 7) y
  lista, campo de texto y pestañas (E-UI3, sección 8).
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
- Las generaciones de `UiDraw` y `UiAtlas` salen de un contador único del proceso, como las de
  los targets. Dos listas o dos atlas distintos nunca tienen la misma, así que el pase no se
  queda con los píxeles viejos si el juego reemplaza el atlas por otro armado igual.
- Liga una sola textura y la lee con `textureLoad`, sin sampler, uniform ni immediates
  (`ui.wgsl`). Corre en Low y en GL 3.3.
- El mapeo de cada quad viaja en enteros relativos a su primer píxel dibujado, no a su origen:
  el texel es exacto aunque el origen esté muy fuera de la pantalla. La instancia ocupa 64 bytes
  (el plan decía 48 con el origen en 16 bits).
- Lo opaco sale igual a `raster`, byte a byte, en High, Mid y Low (también Low en GL). Cada
  mezcla translúcida puede redondear un paso distinto, porque el mezclador de la GPU no usa
  nuestras tablas: un píxel puede quedar tantos pasos lejos como capas translúcidas tiene encima
  (`tests/gpu_ui.rs`). Los tiers de una misma GPU dan los mismos bytes.

### 7. Texto y métodos de entrada (E-UI4)

- `TextInput` es un solo flujo en orden con lo que lee un campo de texto. `TextInput::Text` trae
  el texto de cada tecla, con la distribución del teclado y las teclas muertas ya aplicadas: en
  X11 y Wayland, "´" y "e" llegan como "é". `TextInput::Key` trae las teclas que no escribieron
  nada: Enter, Retroceso, Tab, Esc y Supr (sus caracteres de control se quitan), las flechas,
  Inicio y Fin. Va todo junto porque con dos colas un campo no distingue "x, Retroceso, y" de
  "Retroceso, x, y" cuando caen en el mismo frame. Con Ctrl o Super la tecla no escribe, porque
  es un atajo (winit da "c" para Ctrl+C), y llega como `Key`. Ctrl+Alt sí escribe: es AltGr en
  Windows. Una tecla mantenida se repite con la repetición del sistema. Los controles del juego
  siguen leyendo `KeyboardInput`.
- Un método de entrada (IME, por ejemplo para chino o japonés) manda `ImeEvent::Preedit` con lo
  que se está componiendo. Lo confirmado llega como `TextInput::Text`, en orden con las teclas,
  y también como `ImeEvent::Commit`. Un campo de texto lee solo `TextInput`.
- `ImeOptions { allowed, cursor_area }` se aplica en el hilo principal junto a `CursorOptions`.
  Va apagado por defecto, porque mientras un IME compone las teclas van a él. En X11, Wayland y
  macOS winit no las manda. En Windows sí, sin texto y como `NamedKey::Process`, y el runner las
  deja fuera de `TextInput`: si no, Retroceso borraría texto ya escrito y Enter enviaría el campo
  antes de que llegue lo confirmado. La interfaz lo prende solo con un campo de texto enfocado.
  Cuando el IME arranca de nuevo (`Ime::Enabled`) el runner vuelve a mandar `cursor_area`,
  porque Wayland la olvida cada vez que la ventana recupera el foco.
- `CursorLeft`: `CursorPosition` pasa a `None` cuando el cursor sale de la ventana, y nada queda
  resaltado. El runner lo manda justo antes del update y solo si el cursor no volvió. Si sale y
  vuelve en el mismo frame, vale la posición con la que winit avisa la vuelta, así que un clic
  en ese frame no se pierde.

### 8. Lista, campo de texto y pestañas (E-UI3)

- `list` es la lista de mundos de Minecraft: filas de alto fijo, selección, rueda (media fila por
  línea), barra de desplazamiento que se arrastra y doble clic (dos clics en la misma fila en
  menos de 250 ms de tiempo real). Con foco, arriba y abajo mueven la selección y Enter la abre.
  `scroll_list` es lo mismo sin selección, para páginas de opciones con widgets en las filas.
- La lista llama a la función de fila solo para las filas que se ven y una más de cada lado.
  Mientras el foco está en el widget de una fila, llama también a esa fila y a las que van desde
  ella hasta la más cercana de cada lado con algo que enfocar. Así Tab sigue fila por fila desde
  el widget con foco, aunque la rueda lo haya sacado de la vista, y salta las filas sin nada que
  enfocar (un título, una opción desactivada). El costo crece con las filas que salta, no con el
  largo de la lista. La lista se mueve para mostrar el widget que enfoca el teclado.
- `text_field` lee `TextInput`, así que escribe acentos, ñ y lo que confirma un IME, que se ve
  subrayado en el cursor mientras se compone. Tiene cursor, selección con Shift, con el mouse y
  arrastrando, Ctrl para saltar palabras, Ctrl+A, un máximo de caracteres y un texto de ayuda
  cuando está vacío. Enter da `submitted`. La interfaz prende el IME solo mientras un campo
  tiene el foco.
- Las teclas de navegación llegan por `KeyboardInput` y el texto por `TextInput`, dos colas
  separadas. La interfaz las alinea contando las teclas que van en las dos (Tab, flechas y
  Enter): "ab", Tab y "12" en un mismo frame terminan en los dos campos correctos.
- `tabs`: una pestaña por etiqueta, cada una con su foco. Izquierda y derecha abren la vecina;
  Ctrl+Tab y Ctrl+Shift+Tab recorren las de la capa modal.
- El tiempo real sale de `Time`: el doble clic y el parpadeo del cursor (cada 300 ms). `UiTester`
  mueve su propio reloj, así los tests son exactos.
- Todavía sin portapapeles: necesita una dependencia nueva y el visto bueno del dueño.

## Consecuencias

- `boxel_window` suma `CursorPosition`: la última posición del cursor en píxeles físicos.
- La lista de pintura y la entrada funcionan sin ventana, y `UiTester` hace clic por etiqueta en
  los tests. Con `render` y la feature `ui`, la interfaz se ve en pantalla.
- Lo que sigue: E-UI5 (sprites y fuentes de los packs).
- Las capas `Screen` y `Popup` se vacían en cada frame que nadie las dibuja, así que una pantalla
  cerrada desaparece sola. El HUD se vacía cuando cambia la escala o el tamaño de la ventana: el
  juego lo redibuja con `resource_changed::<GuiScale>`.
- Los paneles translúcidos se ven un poco más claros que en Minecraft, porque mezclamos en luz
  lineal. Su alfa se ajusta a ojo.
