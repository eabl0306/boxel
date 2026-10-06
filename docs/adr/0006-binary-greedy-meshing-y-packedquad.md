# ADR 0006: binary greedy meshing y PackedQuad de 8 bytes

- Estado: aceptado (revisado 2026-10-05: 8 buckets, ver ADR 0012; enmendado 2026-10-06 por
  ADR 0015: `mat` v2 y fork del merge, se aplica en la fase P2)
- Fecha: 2026-10-04

## Contexto

binary-greedy-meshing 0.5.2 (MIT) es 25 a 48x más rápido que block-mesh greedy en terreno
(~23 µs por chunk de 32³). Su salida (`Quad` de 64 bits) tiene convenciones distintas por cara y
no sirve directo para la GPU. Todos los tiers deben compartir los mismos bytes.

## Decisión

- Envolver `binary_greedy_meshing::Mesher<32>` (no fork en M0) y convertir cada quad a un
  `PackedQuad` canónico: `geo = x:5|y:5|z:5|(w-1):5|(h-1):5|face:3|res:4`,
  `mat = capa:12|ao:8|cielo:4|bloque:4|flags:4`. Esquina mínima del bloque, ejes u/v canónicos por
  cara, el plano lo suma el shader.
- Conversión con `Face::vertices_packed` (min/max de los 4 vértices; -1 en el eje normal para caras
  positivas). Validada contra un oráculo de fuerza bruta.
- Buckets (ADR 0012): 6 opacos por cara + 1 cutout + 1 translúcido, contiguos por chunk.

## Consecuencias

- Un solo formato para High, Mid (storage buffer) y Low (atributo de instancia).
- Sin AO en M0 (bgm no lo tiene). M1: AO en la clave de merge (+36 a +65 % de quads) o en el shader.
- Dependencia de un crate pequeño: oráculo de tests y posibilidad de fork in-repo.

## Alternativas descartadas

- block-mesh: mucho más lento.
- Meshing en GPU (compute) en M0: complejidad y no hace falta (el worldgen domina).
- Vértices clásicos (posición, normal, uv): 4 a 8x más memoria y sin vertex pulling.

## Enmienda (2026-10-06, ADR 0015)

Se aplica en la fase P2 de la luz; hasta entonces el código sigue con el formato de arriba.

- **`PackedQuad` v2, sigue en 8 bytes.** `geo` usa sus 4 bits reservados como flags (bits 28 a
  31). `mat = capa:12 | ao:8 | índice de paleta de luz:12`: desaparecen cielo, bloque y flags de
  `mat`. Constantes nuevas `GEO_FLAGS_SHIFT = 28`, `MAT_LIGHT_SHIFT = 20` y
  `MAT_LIGHT_MASK = 0xFFF`; se quitan `MAT_SKY_SHIFT`, `MAT_BLOCK_SHIFT` y `MAT_FLAGS_SHIFT`.
- **Paleta de luz por chunk**: hasta 4096 entradas `[u32; 2]` con la luz de las 4 esquinas, en
  orden de primera aparición. `ChunkMesh` suma `palette` y un `hash` de quads y paleta.
- **AO en la clave del merge** (la opción de M1 de arriba), sin AO en las caras con las 4
  esquinas a oscuras.
- **Fork en el repo**: el merge compara una clave por cara (material, AO, índice de paleta) en
  lugar del `u16` del vóxel, así que `src/greedy.rs` pasa a ser un fork de
  binary-greedy-meshing 0.5.2 (aviso MIT incluido). El crate queda como dependencia de desarrollo:
  con AO apagado y sin luz, los quads del fork son idénticos byte a byte a los de bgm.
