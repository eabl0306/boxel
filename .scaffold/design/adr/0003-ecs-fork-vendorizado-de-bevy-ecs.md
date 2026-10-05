# ADR 0003: ECS como fork vendorizado y recortado de bevy_ecs

- Estado: aceptado (revisado 2026-10-05)
- Fecha: 2026-10-04

## Contexto

El dueño pide un ECS "propio" sin arrastrar dependencias de otro motor, con forks permitidos si la
licencia lo deja (bevy y hecs son MIT OR Apache-2.0). Se compararon tres opciones con prototipos:
A) ECS desde cero (4 a 6 meses hasta paridad, mayor riesgo de UB), B) fork de bevy_ecs (verificado:
compila con 0 ediciones de lógica, 0 crates bevy_*), C) fork de hecs + capa propia (5 a 8 semanas
más; dispatch 4.4x más lento con muchos arquetipos; sin change detection).

## Decisión

Opción "B híbrida" (8/10):

- `vendor/` = fork renombrado de bevy_ecs 0.20.0-rc.2 y dependencias internas (7 crates
  `boxel_*`), con `bevy_platform` reemplazado por un shim, sin reflect ni hotpatching, más el
  backport de bevy#25940. Es su propio workspace de Cargo, con su `Cargo.lock` versionado.
- Generado por `tools/refork/refork.sh`: fuentes ubicadas con `cargo metadata`, renombres con
  verificación (el renombre de la resolución de derives falla en voz alta si upstream cambia),
  parches solo de tests y backports. Diff de librería cero **comprobado** en cada regeneración
  (`check_test_only.py`); un backport que ya está upstream hace fallar el script con "borrarlo".
  Cada archivo `.rs` modificado lleva una línea de aviso (Apache-2.0 §4(b)).
- Todo lo de boxel fuera de `vendor/`. El código del motor usa una superficie curada del ECS
  (`tools/ecs_surface.py`, que también detecta imports agrupados y rutas `boxel::ecs::`).
- Re-fork sobre 0.20.0 final apenas salga (a más tardar, criterio de entrada de M1); después como
  máximo anual; cherry-pick de fixes unsound/UB; Miri en cada PR que toca el fork y semanal;
  NOTICE y licencias por crate; ningún crate `bevy*` en el árbol (cargo-deny + `tools/no_bevy.py`).

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
