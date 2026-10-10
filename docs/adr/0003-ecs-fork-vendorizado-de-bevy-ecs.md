# ADR 0003: ECS como fork vendorizado y recortado de bevy_ecs

- Estado: aceptado (revisado 2026-10-05 y 2026-10-09: re-fork sobre 0.20.0 final, issue #24)
- Fecha: 2026-10-04

## Contexto

El dueño pide un ECS "propio" sin arrastrar dependencias de otro motor, con forks permitidos si la
licencia lo deja (bevy y hecs son MIT OR Apache-2.0). Se compararon tres opciones con prototipos:
A) ECS desde cero (4 a 6 meses hasta paridad, mayor riesgo de UB), B) fork de bevy_ecs (verificado:
compila con 0 ediciones de lógica, 0 crates bevy_*), C) fork de hecs + capa propia (5 a 8 semanas
más; dispatch 4.4x más lento con muchos arquetipos; sin change detection).

## Decisión

Opción "B híbrida" (8/10):

- `vendor/` = fork renombrado de bevy_ecs 0.20.0 (hasta el 2026-10-09, 0.20.0-rc.2) y
  dependencias internas (7 crates `boxel_*`), con `bevy_platform` reemplazado por un shim, sin
  reflect ni hotpatching, más el backport de bevy#25940. Es su propio workspace de Cargo, con su
  `Cargo.lock` versionado.
- Generado por `tools/refork/refork.sh`: fuentes ubicadas con `cargo metadata`, renombres con
  verificación (el renombre de la resolución de derives falla en voz alta si upstream cambia),
  parches solo de tests y backports. Diff de librería cero **comprobado** en cada regeneración
  (`check_test_only.py`); un backport que ya está upstream hace fallar el script con "borrarlo".
  Cada archivo `.rs` modificado lleva una línea de aviso (Apache-2.0 §4(b)).
- Todo lo de boxel fuera de `vendor/`. El código del motor usa una superficie curada del ECS
  (`tools/ecs_surface.py`, que también detecta imports agrupados y rutas `boxel::ecs::`).
- Re-fork sobre 0.20.0 final apenas salga (a más tardar, criterio de entrada de M1): hecho el
  2026-10-09, ver abajo. Después, como máximo uno al año; cherry-pick de fixes unsound/UB; Miri en
  cada PR que toca el fork y semanal; NOTICE y licencias por crate; ningún crate `bevy*` en el
  árbol (cargo-deny + `tools/no_bevy.py`).

## Re-fork sobre 0.20.0 (2026-10-09)

bevy 0.20.0 salió en crates.io el 2026-10-08. `tools/refork/UPSTREAM_VERSION` pasa a `0.20.0` y
`vendor/` se regeneró con el script, sin tocar nada a mano. El diff de `vendor/` es exactamente el
de upstream entre rc.2 y la versión final, más las fechas y versiones de `UPSTREAM.md` y del shim
de plataforma:

- `EntityLocation` gana `#[repr(align(8))]` (mejor codegen alrededor de `UnsafeEntityCell`).
- `#[inline(always)]` en `fetch_next_aliased_unchecked` y `fetch_next_back_aliased_unchecked`, el
  bucle interno de la iteración de queries.
- `replace_related` y `replace_children` quitan duplicados y dejan la primera aparición; upstream
  suma el test `replace_children_duplicates`.

Ninguna dependencia cambió de versión: los manifiestos de `template/` y los dos `Cargo.lock`
quedan iguales. Tampoco hizo falta adaptar ningún crate de boxel: no usan `replace_related`,
`replace_children` ni `EntityLocation`, y la superficie curada no cambia.

**Backport de bevy#25940.** Se conserva. Upstream lo integró en `main` el 2026-09-29 sin
milestone de 0.20, y `index_set.rs` es idéntico en rc.2 y en 0.20.0, así que la versión final no
lo trae. El parche aplica limpio y convive con el nuevo `replace_related`, que usa
`EntityIndexSet::from_iter` y `difference` (ninguno de los dos depende del `DerefMut` que el parche
quita). Cuando un re-fork futuro (0.20.x o 0.21) lo incluya, el script fallará pidiendo borrarlo.
De los demás PR de upstream marcados `I-Unsound`, el último anterior (bevy#25656, observers) ya
está en 0.20.0: su diff aplica en reversa sobre las fuentes (`git apply -R --check`).

**Comprobado:** `just test-vendor` (962 tests de librería de `boxel_ecs`, uno más que con rc.2, y
480 doctests), `just miri` (958 tests de `boxel_ecs` y 11 de `boxel_tasks`, sin UB, unos 20
minutos), `tools/ecs_surface.py`, `tools/no_bevy.py`, `tools/vendor_lock_sync.py`, `cargo deny
check` y todas las compuertas de `AGENTS.md` sobre el workspace raíz (1049 tests).

**Rendimiento.** El alineamiento de `EntityLocation` y los `inline(always)` tocan caminos
calientes (la ubicación de entidades y la iteración de queries). Upstream los hizo para ganar
velocidad, pero aquí no se midieron: quedan para la próxima ronda de control.

## Consecuencias

- Ergonomía y rendimiento de Bevy hoy (change detection, sparse sets, ejecutor paralelo,
  mensajes, observers).
- Poseemos ~48k líneas con ~2.2k bloques unsafe que no auditamos: dependemos de upstream y Miri.
- `ParamSet` necesita `&'static` explícito (el parche de rustc solo reconoce `bevy_ecs`); el error
  es E0477 "the type `&mut A` does not fulfill the required lifetime".
- Salida de emergencia, con su alcance real: un núcleo propio podría reemplazar al fork detrás de
  la superficie curada **del código del motor**. La fachada re-exporta `boxel_ecs` completo y los
  derives de los usuarios expanden a rutas internas, así que un cambio de núcleo rompería el código
  de usuarios que use derives o items fuera del prelude. Por eso no es una salida barata, es una
  salida posible.

## Alternativas descartadas

- Depender de `bevy_ecs` desde crates.io: viola "cero crates bevy_*".
- ECS desde cero: meses antes de paridad y mayor riesgo de soundness.
- hecs + capa: falta change detection y el ejecutor paralelo; el ahorro de complejidad no compensa.
- git subtree del repo de Bevy: arrastra historia y no aplica el recorte.
