# ADR 0058: la niebla termina en el agua y el vidrio

- Estado: aceptado (el dueño: "este si hay que arreglarlo", 2026-10-31; issue #139)
- Fecha: 2026-10-31

## Contexto

La marcha de la niebla y las nubes (ADR 0030) llega hasta la profundidad del pase principal. El
pase translúcido del terreno (agua, vidrio, hielo, portal del nether) no escribe profundidad, así
que donde detrás de un agua o un vidrio está el cielo, la marcha sigue hasta el infinito y las
nubes de atrás se pintan encima, sin tinte. Se veía en el agua de un lago visto desde una loma,
en las ventanas y en el portal. El ADR 0055 lo resolvió solo para los quads de celda.

Lo que pesaba:

- **El tier Low.** Un solo bloque uniforme y una sola textura por pipeline. La marcha ya lee una
  textura (la profundidad); no puede leer una segunda.
- **Los mismos píxeles.** Sin niebla, nada puede cambiar. Con niebla, lo que no tiene agua ni
  vidrio delante tampoco.
- **El costo en la 780M.** La niebla es opcional y no tiene que entrar en 144 Hz en la 780M (ADR
  0030, sección 6), pero no debe costar mucho más.
- **Cómo se ve en Minecraft.** El agua y el vidrio de color escriben profundidad: la niebla de
  delante los cubre según su propia distancia, y las nubes de atrás no se ven a través de ellos.
  El vidrio común es cutout: su interior transparente deja ver las nubes.

## Decisión

Una profundidad translúcida para la niebla, en la misma profundidad principal:

- Después del color del pase translúcido, y solo en un frame que marcha niebla, los buckets
  translúcidos se dibujan otra vez con una pipeline que escribe profundidad y ningún color
  (máscara vacía, como el prepase del agua lejana del ADR 0031). El color ya está terminado, así
  que ningún píxel cambia; solo la profundidad que lee la marcha queda en la superficie
  translúcida más cercana.
- El fragmento, `fs_translucent_depth` (en `voxel_pull.wgsl` y `voxel_instanced.wgsl`), descarta
  los texeles con alfa menor a 0,1, el umbral cutout de Minecraft: el interior casi transparente
  del vidrio deja pasar la niebla y las nubes. El marco y las vetas la terminan. El agua, el
  hielo, los vidrios de color, el portal, la miel y el slime de los dos packs tienen alfa de
  0,4 a 0,9 en todos sus texeles: la terminan entera.
- La pipeline se crea con la primera entrada de niebla (`VoxelRenderer::set_fog`, dentro de los
  mismos scopes de error que los pases de la niebla) y se suelta con `set_fog(None)`. Sin niebla
  no existe, igual que todo lo demás de la niebla. El smoke de la niebla la compila.
- Tiene el mismo layout que la pipeline translúcida en cada tier: un uniform y un par
  textura/sampler en Low.
- `RenderStats::fog_depth_draws` cuenta sus draws (fuera de `draws`).

### Lo que se ve

- **Nubes detrás del agua o del vidrio:** no se pintan encima. Se ve el agua o el vidrio con el
  cielo de fondo, como en Minecraft.
- **Nubes o niebla delante del agua:** se siguen viendo. La marcha llega hasta la superficie, así
  que el agua queda cubierta por la niebla que hay hasta ella: su propia distancia.
- **Bajo el agua:** mirando hacia arriba, la marcha termina en la superficie vista desde abajo.
  Las nubes ya no se pintan sobre ella.
- **A través del vidrio común:** el interior transparente deja ver las nubes y la niebla de
  atrás. A través del vidrio de color, el agua o el hielo no: la niebla de detrás se pierde, como
  en Minecraft. La niebla de distancia del terreno (issue #51) es por fragmento y no cambia.
- **De lejos o de canto,** los mips del vidrio común mezclan el marco con el interior y el alfa
  pasa de 0,1: el vidrio lejano termina la niebla entero. Se nota poco: a esa distancia el vidrio
  ya se ve como un velo.

## Medidas

- `headless_world --view-distance 8`: los mismos quads que en main (112.316; el mesher no
  cambió).
- Sin criterion: no cambia ningún camino de CPU caliente (un draw más por bucket translúcido
  visible, solo con niebla).
- `just perf-smoke surface forest` (sin niebla, RTX): superficie 2.109 a 2.102 fps (-0,3 %;
  la superficie varía unos ±10 % de una corrida a otra con cualquier binario), bosque 3.608 a
  3.662 (+1,5 %), p99 igual o mejor. Ok.
- `just perf-smoke rtx/surface 780m/surface -- --fog both`: RTX 644 a 651 fps (+0,9 %), p99 4,63
  a 4,58 ms. Ok. En la 780M el harness no midió ningún lado: con la niebla, los primeros
  segundos de las dos corridas muestran menos de 50 frames y las descarta (base 200 fps, p99
  12,4 ms; nueva 196 fps, p99 12,3 ms, las dos marcadas con ruido). La niebla en la 780M no
  tiene que entrar en 144 Hz (ADR 0030) y la diferencia está dentro del ruido.
- `gpu_fog` (prueba nueva, en High, Mid y Low de la 780M, la RTX 5050, llvmpipe y Low en GL de
  la RTX): un muro de agua delante de las nubes no tiene ningún píxel pintado por ellas (antes,
  todos); un muro de vidrio común deja ver las nubes por su interior y las tapa con el marco;
  bajo un lago la imagen es la de sin niebla (antes, 27.659 píxeles cambiaban); sobre un lago
  cada texel de la marcha es la marcha de referencia hasta la superficie del agua (antes
  difería en 1.427 de 14.265 texels); los tres tiers dan los mismos píxeles. Las pruebas de
  niebla de siempre pasan sin cambios.
- `gpu_lod`: la bruma del valle sobre un LOD vacío ya no llega a los texels que miran a través
  del muro de agua de la escena (terminan en él); la prueba los deja fuera.

## Alternativas descartadas

- **La niebla antes del pase translúcido.** No cuesta nada más, pero el agua se dibuja encima de
  la niebla que tiene delante: unas nubes entre el ojo y un lago (desde arriba de ellas) o un
  banco de niebla sobre el agua desaparecían. Romper esto era peor que el problema.
- **Las dos cosas a la vez, exacto:** la niebla de detrás del translúcido antes de él y la de
  delante después, con dos profundidades. La marcha tendría que leer dos texturas (Low lee una),
  guardar dos resultados y componer dos veces. Más objetivos, más pases y más costo en la 780M
  para ver nubes a través del vidrio de color, que Minecraft tampoco muestra.
- **Que la pipeline translúcida escriba profundidad.** Ahorra el segundo dibujo, pero cambia el
  color: un agua o un vidrio mal ordenado un frame (el orden en segundo plano, ADR 0018) taparía
  lo que tiene detrás, también sin niebla.

## Consecuencias

- La niebla y las nubes respetan el agua, el vidrio, el hielo y el portal del terreno, en los
  tres tiers y con los mismos píxeles en todos.
- Sin niebla, el motor es el de antes: ni pipeline ni draws nuevos.
- Con niebla, cada frame dibuja otra vez los quads translúcidos visibles, solo en profundidad.
- El límite "Translúcidos" del ADR 0030 queda así: la niebla de detrás de un translúcido que la
  termina no se ve. El valle del ejemplo empieza sobre el mar y no cambia.
