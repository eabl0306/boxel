use crate::App;
use std::any::Any;

/// A unit of app configuration. `build` runs immediately in `App::add_plugins`.
pub trait Plugin: Any + Send + Sync {
    /// Configures the app (resources, systems, messages, schedules).
    fn build(&self, app: &mut App);
    /// Polled until true before `finish` (async setup such as GPU init may report false).
    fn ready(&self, _app: &App) -> bool {
        true
    }
    /// Runs once after every plugin is ready (late wiring that needs other plugins' resources).
    fn finish(&self, _app: &mut App) {}
    /// Runs once after `finish`.
    fn cleanup(&self, _app: &mut App) {}
    /// Name used for uniqueness checks and logs.
    fn name(&self) -> &str {
        std::any::type_name::<Self>()
    }
    /// If true (default), adding it twice panics.
    fn is_unique(&self) -> bool {
        true
    }
}

impl<T: Fn(&mut App) + Send + Sync + 'static> Plugin for T {
    fn build(&self, app: &mut App) {
        self(app);
    }
}

pub(crate) mod sealed {
    use crate::{App, Plugin, PluginGroup};
    use variadics_please::all_tuples;

    pub trait Plugins<Marker> {
        fn add_to_app(self, app: &mut App);
    }
    pub struct PluginMarker;
    pub struct PluginGroupMarker;
    pub struct PluginsTupleMarker;

    impl<P: Plugin> Plugins<PluginMarker> for P {
        fn add_to_app(self, app: &mut App) {
            app.add_boxed_plugin(Box::new(self));
        }
    }

    impl<P: PluginGroup> Plugins<PluginGroupMarker> for P {
        fn add_to_app(self, app: &mut App) {
            self.build().finish(app);
        }
    }

    macro_rules! impl_plugins_tuples {
        ($(#[$meta:meta])* $(($param: ident, $plugins: ident)),*) => {
            $(#[$meta])*
            impl<$($param, $plugins),*> Plugins<(PluginsTupleMarker, $($param,)*)> for ($($plugins,)*)
            where $($plugins: Plugins<$param>),*
            {
                #[allow(non_snake_case, unused_variables, reason = "macro-generated tuple impl")]
                fn add_to_app(self, app: &mut App) {
                    let ($($plugins,)*) = self;
                    $($plugins.add_to_app(app);)*
                }
            }
        }
    }

    all_tuples!(impl_plugins_tuples, 0, 15, P, S);
}

/// Anything `App::add_plugins` accepts: a [`Plugin`], a [`PluginGroup`](crate::PluginGroup) or a tuple of them.
pub trait Plugins<Marker>: sealed::Plugins<Marker> {}
impl<Marker, T: sealed::Plugins<Marker>> Plugins<Marker> for T {}
