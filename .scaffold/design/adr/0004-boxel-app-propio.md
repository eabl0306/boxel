# ADR 0004: boxel_app propio (App, plugins, schedules, runner, tiempo)

- Estado: aceptado (revisado 2026-10-05)
- Fecha: 2026-10-04

## Contexto

`bevy_app` no se vendoriza: arrastra reflect y conceptos que no necesitamos (SubApps con
extracción genérica, hotpatch, escenas). El fork del ECS ya provee schedules, mensajes y
ejecutores. La API de App es chica. La revisión encontró detalles de semántica que Bevy resuelve y
el borrador no: mensajes perdidos por lectores en `FixedUpdate`, `Time` sin vista fija dentro del
paso fijo, `should_exit` que devolvía el último mensaje, plugins agregados durante `finish` que se
perdían y un desmontaje sin orden al salir.

## Decisión

Crate `boxel_app` propio, inspirado en `bevy_app` 0.20:

- `App` (un `World`), `Plugin` (`build/ready/finish/cleanup`), `Plugins<Marker>` para plugins,
  grupos y tuplas, `PluginGroup`/`PluginGroupBuilder`, `insert_non_send`/`init_non_send`.
- `Main` con `MainScheduleOrder` (`First`..`Last`, insertables), startup una vez, `FixedMain` a
  64 Hz con acumulador, y `Shutdown`, que los runners corren una vez al salir (`App::shutdown`)
  con la ventana viva.
- Semántica de Bevy donde importa: la rotación de mensajes espera a que corra un paso fijo
  (`signal_message_update_system`); dentro de `FixedMain`, `Res<Time>` es la vista fija;
  `should_exit` devuelve el primer error; los plugins agregados durante `finish` quedan
  registrados.
- `Time`, `FixedTime`, `TaskPoolPlugin` (el hilo principal cuenta: workers = paralelismo - 1;
  IO 10 %, async 40 % hasta 8, compute el resto), `ScheduleRunnerPlugin` headless, `AppExit` como
  mensaje.
- Sin SubApps en M0. El render corre en el mismo mundo dentro de `Main` (schedule `Render` después
  de `Last`, ejecutor single-thread); hay un paso de extract para mover el render a otro hilo en M3.
- El tiempo, los task pools y la mayor parte de `App` vienen implementados y probados en el
  esqueleto, porque los usan casi todos los demás paquetes.

## Consecuencias

- API familiar para usuarios de Bevy, sin código de Bevy fuera de `vendor/` (salvo el truco de
  tuplas de plugins, atribuido en NOTICE).
- Mantenimiento propio de ~700 líneas, con tests de contrato para cada comportamiento de arriba.

## Alternativas descartadas

- Vendorizar `bevy_app`: dependencias de reflect y más superficie que mantener.
- App mínima sin plugins: perdería la composición estilo Bevy que es objetivo del proyecto.
