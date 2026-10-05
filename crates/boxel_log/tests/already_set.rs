//! A second `init_logging` reports `AlreadySet`, and `LogPlugin` tolerates it. Own binary: the
//! global subscriber cannot leak into other tests.
use boxel_app::App;
use boxel_log::{DEFAULT_FILTER, LogInitError, LogPlugin, init_logging};

#[test]
fn second_init_returns_already_set() {
    init_logging(DEFAULT_FILTER).expect("first init installs the subscriber");
    assert!(matches!(
        init_logging("debug"),
        Err(LogInitError::AlreadySet)
    ));
    assert!(matches!(
        init_logging(DEFAULT_FILTER),
        Err(LogInitError::AlreadySet)
    ));
    // The plugin only logs at debug level when a subscriber exists.
    let mut app = App::new();
    app.add_plugins(LogPlugin::default());
    app.update();
    tracing::info!("logging still works");
}
