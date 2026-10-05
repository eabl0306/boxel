use crate::plugin::{Plugin, Plugins};
use crate::schedules::{First, Main, MainSchedulePlugin, Shutdown};
use boxel_ecs::message::{
    MessageRegistry, MessageUpdateSystems, message_update_condition, message_update_system,
};
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::{InternedSystemSet, ScheduleLabel};
use boxel_ecs::system::ScheduleSystem;
use std::any::Any;
use std::collections::HashSet;
use std::num::NonZeroU8;

/// The runner owns the app after [`App::run`] and drives [`App::update`].
pub type RunnerFn = Box<dyn FnOnce(App) -> AppExit>;

/// Lifecycle of plugin building. `App::run` runners must call [`App::finish`] and
/// [`App::cleanup`] once [`App::plugins_state`] reports [`PluginsState::Ready`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginsState {
    /// Plugins are still being added (or some `Plugin::ready` returned false).
    Adding,
    /// All plugins report ready.
    Ready,
    /// `finish` has run.
    Finished,
    /// `cleanup` has run.
    Cleaned,
}

/// Request to exit, written as a message by any system. The runner reads it after each update.
#[derive(Message, Debug, Clone, Default, PartialEq, Eq)]
pub enum AppExit {
    /// Clean exit (process exit code 0).
    #[default]
    Success,
    /// Exit with a non-zero code.
    Error(NonZeroU8),
}

impl AppExit {
    /// `AppExit::Error(1)`.
    pub const fn error() -> Self {
        Self::Error(NonZeroU8::MIN)
    }

    /// Exit code `code` (`0` = success).
    pub fn from_code(code: u8) -> Self {
        NonZeroU8::new(code).map_or(Self::Success, Self::Error)
    }

    /// True for [`AppExit::Success`].
    pub const fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }

    /// Process exit code.
    pub const fn code(&self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Error(c) => c.get(),
        }
    }
}

impl std::process::Termination for AppExit {
    fn report(self) -> std::process::ExitCode {
        std::process::ExitCode::from(self.code())
    }
}

/// The application: one ECS `World` plus plugins, schedules and a runner.
pub struct App {
    world: World,
    plugins: Vec<Box<dyn Plugin>>,
    plugin_names: HashSet<String>,
    plugin_build_depth: usize,
    plugins_state: PluginsState,
    runner: Option<RunnerFn>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// An app with the main schedule structure ([`MainSchedulePlugin`]), message rotation in
    /// `First` and the [`AppExit`] message registered. No time, task pools or window: add
    /// `DefaultPlugins`/`MinimalPlugins` (facade) or the individual plugins.
    pub fn new() -> Self {
        let mut app = Self::empty();
        app.world.resource_mut::<Schedules>().add_systems(
            First,
            message_update_system
                .in_set(MessageUpdateSystems)
                .run_if(message_update_condition),
        );
        app.add_plugins(MainSchedulePlugin);
        app.add_message::<AppExit>();
        app
    }

    /// An app with an empty world (only the `Schedules` resource). Mostly for tests.
    pub fn empty() -> Self {
        let mut world = World::new();
        world.init_resource::<Schedules>();
        Self {
            world,
            plugins: Vec::new(),
            plugin_names: HashSet::new(),
            plugin_build_depth: 0,
            plugins_state: PluginsState::Adding,
            runner: Some(Box::new(run_once)),
        }
    }

    /// The ECS world.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The ECS world, mutably.
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// Runs the `Main` schedule once, then clears change trackers.
    pub fn update(&mut self) {
        self.world.run_schedule(Main);
        self.world.clear_trackers();
    }

    /// Hands the app to its runner and returns the runner's exit status. The default runner
    /// (`run_once`) finishes plugins and calls `update` once.
    pub fn run(&mut self) -> AppExit {
        let mut app = std::mem::replace(self, App::empty());
        let runner = app.runner.take().unwrap_or_else(|| Box::new(run_once));
        runner(app)
    }

    /// Replaces the runner (the window plugin installs the winit runner here).
    pub fn set_runner(&mut self, f: impl FnOnce(App) -> AppExit + 'static) -> &mut Self {
        self.runner = Some(Box::new(f));
        self
    }

    /// Current plugin lifecycle state (polls `Plugin::ready` while `Adding`).
    pub fn plugins_state(&mut self) -> PluginsState {
        if self.plugins_state == PluginsState::Adding
            && self.plugin_build_depth == 0
            && self.plugins.iter().all(|p| p.ready(self))
        {
            self.plugins_state = PluginsState::Ready;
        }
        self.plugins_state
    }

    /// Calls `Plugin::finish` on every plugin, in insertion order. A plugin added from another
    /// plugin's `finish` is built immediately, kept in the registry and finished in the same loop.
    pub fn finish(&mut self) {
        let mut i = 0;
        while i < self.plugins.len() {
            let plugin = std::mem::replace(&mut self.plugins[i], Box::new(PlaceholderPlugin));
            plugin.finish(self);
            self.plugins[i] = plugin;
            i += 1;
        }
        self.plugins_state = PluginsState::Finished;
    }

    /// Calls `Plugin::cleanup` on every plugin, in insertion order (same iteration rule as
    /// [`App::finish`]).
    pub fn cleanup(&mut self) {
        let mut i = 0;
        while i < self.plugins.len() {
            let plugin = std::mem::replace(&mut self.plugins[i], Box::new(PlaceholderPlugin));
            plugin.cleanup(self);
            self.plugins[i] = plugin;
            i += 1;
        }
        self.plugins_state = PluginsState::Cleaned;
    }

    /// Runs the [`Shutdown`] schedule once (no-op if nothing was added to it). Runners call this
    /// exactly once after the last update, while the event loop (if any) is still alive, so
    /// plugins release GPU and window resources in a defined order and flush state.
    pub fn shutdown(&mut self) {
        let _ = self.world.try_run_schedule(Shutdown);
    }

    /// Adds one plugin, a plugin group or a tuple of them.
    ///
    /// # Panics
    /// If a unique plugin (`Plugin::is_unique`) is added twice, or if called after `finish`.
    pub fn add_plugins<M>(&mut self, plugins: impl Plugins<M>) -> &mut Self {
        assert!(
            matches!(
                self.plugins_state,
                PluginsState::Adding | PluginsState::Ready
            ),
            "plugins cannot be added after App::finish"
        );
        plugins.add_to_app(self);
        self
    }

    pub(crate) fn add_boxed_plugin(&mut self, plugin: Box<dyn Plugin>) {
        if plugin.is_unique() && !self.plugin_names.insert(plugin.name().to_string()) {
            panic!("plugin {} was already added", plugin.name());
        }
        self.plugin_build_depth += 1;
        plugin.build(self);
        self.plugin_build_depth -= 1;
        self.plugins.push(plugin);
    }

    /// True if a plugin of type `T` was added.
    pub fn is_plugin_added<T: Plugin>(&self) -> bool {
        self.plugins
            .iter()
            .any(|p| (p.as_ref() as &dyn Any).is::<T>())
    }

    /// All added plugins of type `T`.
    pub fn get_added_plugins<T: Plugin>(&self) -> Vec<&T> {
        self.plugins
            .iter()
            .filter_map(|p| (p.as_ref() as &dyn Any).downcast_ref::<T>())
            .collect()
    }

    /// Adds systems to a schedule (created on demand).
    pub fn add_systems<M>(
        &mut self,
        schedule: impl ScheduleLabel,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self {
        self.world
            .resource_mut::<Schedules>()
            .add_systems(schedule, systems);
        self
    }

    /// Configures system sets in a schedule (created on demand).
    pub fn configure_sets<M>(
        &mut self,
        schedule: impl ScheduleLabel,
        sets: impl IntoScheduleConfigs<InternedSystemSet, M>,
    ) -> &mut Self {
        self.world
            .resource_mut::<Schedules>()
            .configure_sets(schedule, sets);
        self
    }

    /// Inserts (or replaces) a schedule.
    pub fn add_schedule(&mut self, schedule: Schedule) -> &mut Self {
        self.world.add_schedule(schedule);
        self
    }

    /// Creates an empty schedule for `label` if missing.
    pub fn init_schedule(&mut self, label: impl ScheduleLabel) -> &mut Self {
        let label = label.intern();
        let mut schedules = self.world.resource_mut::<Schedules>();
        if !schedules.contains(label) {
            schedules.insert(Schedule::new(label));
        }
        self
    }

    /// Runs `f` on the schedule for `label` (created on demand).
    pub fn edit_schedule(
        &mut self,
        label: impl ScheduleLabel,
        f: impl FnOnce(&mut Schedule),
    ) -> &mut Self {
        let label = label.intern();
        let mut schedules = self.world.resource_mut::<Schedules>();
        if !schedules.contains(label) {
            schedules.insert(Schedule::new(label));
        }
        f(schedules.get_mut(label).expect("schedule exists"));
        self
    }

    /// Registers message type `M` (double-buffered, rotated in `First`).
    pub fn add_message<M: Message>(&mut self) -> &mut Self {
        if !self.world.contains_resource::<Messages<M>>() {
            MessageRegistry::register_message::<M>(&mut self.world);
        }
        self
    }

    /// Inserts or replaces a resource.
    pub fn insert_resource<R: Resource>(&mut self, resource: R) -> &mut Self {
        self.world.insert_resource(resource);
        self
    }

    /// Inserts a resource from `FromWorld`/`Default` if missing.
    pub fn init_resource<R: Resource + FromWorld>(&mut self) -> &mut Self {
        self.world.init_resource::<R>();
        self
    }

    /// Inserts or replaces a non-`Send` resource (main thread only; systems read it with
    /// `NonSend<R>`/`NonSendMut<R>` and the executor runs them on the main thread).
    pub fn insert_non_send<R: 'static>(&mut self, resource: R) -> &mut Self {
        self.world.insert_non_send(resource);
        self
    }

    /// Inserts a non-`Send` resource from `FromWorld`/`Default` if missing.
    pub fn init_non_send<R: 'static + FromWorld>(&mut self) -> &mut Self {
        self.world.init_non_send::<R>();
        self
    }

    /// The exit request of this frame, if any [`AppExit`] was written since messages were last
    /// rotated: the FIRST error if any message is an error, else `Success`.
    pub fn should_exit(&self) -> Option<AppExit> {
        let messages = self.world.get_resource::<Messages<AppExit>>()?;
        let mut cursor = messages.get_cursor();
        let mut any = false;
        for exit in cursor.read(messages) {
            if !exit.is_success() {
                return Some(exit.clone());
            }
            any = true;
        }
        any.then_some(AppExit::Success)
    }
}

/// Stands in for a plugin while its own `finish`/`cleanup` runs (the real one is swapped back).
struct PlaceholderPlugin;

impl Plugin for PlaceholderPlugin {
    fn build(&self, _app: &mut App) {}
    fn is_unique(&self) -> bool {
        false
    }
}

/// Default runner: finish plugins, update once.
fn run_once(mut app: App) -> AppExit {
    while app.plugins_state() == PluginsState::Adding {
        std::thread::yield_now();
    }
    app.finish();
    app.cleanup();
    app.update();
    let exit = app.should_exit().unwrap_or_default();
    app.shutdown();
    exit
}
