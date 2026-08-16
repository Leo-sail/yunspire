pub mod plugin;
pub mod storage;
pub mod types;
pub mod validation;

pub use plugin::ConfigPlugin;
pub use types::{
    ConfigQueryRequest, ConfigResponse, ConfigSchema, ConfigType, ConfigUpdateRequest,
    DatabaseConfig, RuntimeSettings,
};

// 公开内部实现供桥接层使用
pub use plugin::{
    get_runtime_settings_impl, update_runtime_settings_impl, update_scheduler_enabled_impl,
};

// 内部使用
pub(crate) use storage::{
    delete_runtime_settings, load_runtime_settings, save_runtime_settings,
    update_scheduler_enabled,
};

pub(crate) use validation::{
    validate_config, validate_database_config, validate_runtime_settings, ValidationError,
};
