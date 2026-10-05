//! What `DEFAULT_FILTER` lets through, checked with a thread-local subscriber (no global state).
use boxel_log::DEFAULT_FILTER;
use tracing::Level;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;

/// Runs `check` with a subscriber filtered by `DEFAULT_FILTER` installed on this thread.
fn with_default_filter<R>(check: impl FnOnce() -> R) -> R {
    let subscriber =
        tracing_subscriber::registry().with(EnvFilter::try_new(DEFAULT_FILTER).expect("parses"));
    tracing::subscriber::with_default(subscriber, check)
}

#[test]
fn sctk_adwaita_warnings_are_silenced_but_its_errors_are_not() {
    with_default_filter(|| {
        // winit's Wayland decorations: `Ignoring unknown button type: ` on GNOME.
        assert!(!tracing::enabled!(target: "sctk_adwaita::buttons", Level::WARN));
        assert!(!tracing::enabled!(target: "sctk_adwaita::title::ab_glyph_renderer", Level::WARN));
        assert!(tracing::enabled!(target: "sctk_adwaita::config", Level::ERROR));
        assert!(tracing::enabled!(target: "sctk_adwaita", Level::ERROR));
    });
}

#[test]
fn everything_else_keeps_its_level() {
    with_default_filter(|| {
        // Engine warnings (e.g. the fixed-step overrun) and info lines stay visible.
        assert!(tracing::enabled!(target: "boxel_app::time", Level::WARN));
        assert!(tracing::enabled!(target: "boxel_render::plugin", Level::INFO));
        assert!(!tracing::enabled!(target: "boxel_render::plugin", Level::DEBUG));
        // A crate whose name only starts like the silenced one is not affected.
        assert!(tracing::enabled!(target: "sctk::shell", Level::WARN));
        // wgpu and naga stay at warn.
        assert!(tracing::enabled!(target: "wgpu_core::device", Level::WARN));
        assert!(!tracing::enabled!(target: "wgpu_hal::vulkan", Level::INFO));
        assert!(!tracing::enabled!(target: "naga::front", Level::INFO));
    });
}
