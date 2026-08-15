pub mod plugin;
pub mod plugin_registry;

pub use plugin::{Capability, Command, Migration, PluginContext, PluginConfig, YunspirePlugin};
pub use plugin_registry::PluginRegistry;
