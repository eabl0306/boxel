# Informe M0

Fecha: 2026-10-05. Máquina: RTX 5050 Laptop + Radeon 780M, Ubuntu con Wayland.

## En resumen

- M0 está terminado. Compila, los tests pasan y clippy y rustdoc están limpios.
- Las 22 pruebas del plan de verificación pasan en esta máquina. Miri y los tests del ECS
  vendorizado también.
- El motor elige solo la GPU y el tier, y los tres tiers dibujan la misma imagen.
- Falta un paso: tu prueba manual (al final).
- Hay dos decisiones para M1: el límite de quads por bucket y el uso de CPU del bucle.

## Qué funciona

- **Elección de GPU**: detecta adaptadores, los clasifica, prueba cada uno y elige el mejor. Si
  uno falla, pasa al siguiente. Recuerda la última GPU que funcionó.
- **Render en tres tiers** (High, Mid, Low) en Vulkan y en OpenGL, con hojas recortadas.
- **Mundo**: genera terreno, mallea y carga chunks en hilos. Editar bloques funciona por código
  (todavía sin interfaz).
- **Ventana**: Wayland y X11. Minimizar no congela la simulación. Redimensionar funciona.
- **Pérdida de GPU**: si el dispositivo se pierde, elige otra vez y sigue dibujando. La prueba
  con `--simulate-device-loss` no deja marca en la memoria de GPUs.
- **Herramientas**: `just ci`, los tests de GPU en todos los adaptadores, Miri y benchmarks.
  Workspace: 310 tests. ECS vendorizado: 1002 tests y 480 doctests; con Miri, 968 tests sin
  errores (20 min).

## Qué GPU elige

| Caso | Elige |
|---|---|
| Por defecto | Vulkan, tier High, RTX 5050 |
| `--power low` | Vulkan, tier High, Radeon 780M (RADV) |
| Con ventana y `WGPU_BACKEND=gl` | GL, tier Mid, Radeon 780M (radeonsi 4.6) |
| GL sin ventana | GL, tier Low, NVIDIA GL 3.3 (Mid pide GL 4.3) |
| `--software` | Vulkan, tier Mid, lavapipe (el software llega como mucho a Mid) |
| EGL de NVIDIA forzado | wgpu falla al iniciar; boxel lo captura y sale con código 2 y un mensaje claro |

Tier máximo por adaptador: RTX y 780M en Vulkan, High. 780M en GL, lavapipe y llvmpipe, Mid.
NVIDIA en GL, Low.

## Imágenes entre tiers

`voxel_world`, 1280x720, misma cámara, 1096 chunks con malla:

- RTX 5050 en Vulkan: Mid y Low son idénticas a High (0 píxeles distintos).
- Radeon 780M en GL: Low es idéntica a Mid.
- Entre GPUs distintas (780M en GL contra RTX en Vulkan): 0.19 % de píxeles, solo en bordes.
- Tests de GPU: en los 6 adaptadores y backends probados, todos los tiers dan la misma imagen.

Las capturas quedan en `shots/` (no se versiona). Se ven colinas verdes con escalones de tierra,
árboles con hojas recortadas y niebla al fondo. Las cinco capturas se ven iguales.

## Medidas

Build release, máquina casi quieta. Son orientativas.

**CPU por frame** en `voxel_world` (todo el proceso, 1096 chunks, 240 mil quads):

| GPU | High | Mid | Low |
|---|---|---|---|
| RTX 5050, Vulkan | 0.15 ms | 0.20 ms | 0.20 ms |
| Radeon 780M, Vulkan | 0.31 ms | 0.29 ms | 0.30 ms |
| Radeon 780M, GL | no aplica | 0.43 ms | 0.47 ms |

En la 780M con High manda la GPU: 0.53 ms por frame.

**Mallado** (un chunk de 32³): 30 µs; leerlo y mallearlo, 70 a 93 µs. Meta: menos de 150 µs.

**Generación de terreno**: un chunk de superficie tarda 48 µs (meta: menos de 0.3 ms). Uno de
aire, 11 µs.

**`headless_world`**: 534 chunks, 102 con malla, 15 997 quads, listo en 2.0 s (124 frames).

**`voxel_world`**: carga 3270 chunks (1096 con malla) en 0.4 a 0.7 s. Elegir la RTX tarda 100
a 150 ms; la 780M, unos 20 ms.

## Qué se recortó y qué queda para M1

No se recortó nada de M0. Los jobs de CI de Windows y macOS están escritos, pero no corren hasta
el primer push. Antes de ese push hay que poner la URL real del repo en `Cargo.toml`
(`repository`, hoy dice `OWNER`).

Para M1, además de lo previsto en el roadmap (agua, AO, raycast y edición, guardado, wgpu 31):

1. **Carga de chunks atada a los frames**: cada slot de trabajo hace un job cada 2 frames. A 60
   Hz el mundo completo tardaría unos 14 s en cargar. Hoy no se nota porque el bucle va muy
   rápido (punto 2).
2. **Bucle interactivo**: sin flags de prueba hace unos 20 000 updates por segundo y ocupa un
   núcleo entero. Hay que pausarlo entre frames. Al hacerlo aparece el punto 1, así que van
   juntos.
3. **Árboles**: viven en el ejemplo `voxel_world`, no en `boxel_worldgen`.

## Riesgos abiertos

1. **Límite de quads por bucket**: con dos materiales recortados alternados, un chunk puede
   pasar el límite (98 304 quads, real hasta 196 608). Pasado el límite, los tres tiers dibujan
   solo los primeros 98 304: faltan hojas. Hoy no pasa: hay un solo material recortado. Arreglo
   simple: subir la constante (el buffer de índices pasa de 2.4 a 4.7 MB).
2. **CPU en uso interactivo**: el punto 2 de arriba. En una laptop gasta batería.
3. **Carrera de datos en `bumpalo`** (dependencia del ECS), encontrada por Miri: dos hilos que
   clonan entidades con componentes vacíos a la vez escriben la misma variable global. Escriben
   el mismo valor, así que en la práctica no rompe nada, pero es un bug. Hay que reportarlo a
   bumpalo. Mientras, `just miri` corre los tests de a uno.
4. **Windows y macOS** no se probaron.

## Prueba manual (para Ezequiel)

Corre `just run voxel_world` y revisa (es normal que un núcleo de CPU vaya al 100 %, ver M1
punto 2):

- [ ] Clic izquierdo captura el mouse. Mouse y WASD mueven la cámara (Espacio sube, Shift baja,
      Ctrl acelera).
- [ ] Escape suelta el cursor.
- [ ] Arrastrar el borde de la ventana la redimensiona y la imagen se adapta.
- [ ] Mover la ventana a otro escritorio 10 s y volver: en la terminal siguen saliendo líneas
      `fps=` y la imagen vuelve.

Si algo falla, copia el comando y la salida de la terminal.
