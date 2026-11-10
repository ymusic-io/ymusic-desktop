//! The typed key-value settings store shared by the UI and the engine bundle (`settings/get|set`).

use std::collections::HashMap;
use std::sync::Mutex;

pub type SettingsError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    String(String),
    Boolean(bool),
    Int(i32),
}

/// Blocking: call off the UI and engine threads.
pub trait SettingsStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<SettingValue>, SettingsError>;
    fn put(&self, key: &str, value: SettingValue) -> Result<(), SettingsError>;

    fn string(&self, key: &str, default: &str) -> Result<String, SettingsError> {
        Ok(match self.get(key)? {
            Some(SettingValue::String(value)) => value,
            _ => default.to_owned(),
        })
    }

    fn boolean(&self, key: &str, default: bool) -> Result<bool, SettingsError> {
        Ok(match self.get(key)? {
            Some(SettingValue::Boolean(value)) => value,
            _ => default,
        })
    }

    fn int(&self, key: &str, default: i32) -> Result<i32, SettingsError> {
        Ok(match self.get(key)? {
            Some(SettingValue::Int(value)) => value,
            _ => default,
        })
    }
}

#[derive(Default)]
pub struct MemorySettings {
    values: Mutex<HashMap<String, SettingValue>>,
}

impl SettingsStore for MemorySettings {
    fn get(&self, key: &str) -> Result<Option<SettingValue>, SettingsError> {
        Ok(self.values.lock().unwrap().get(key).cloned())
    }

    fn put(&self, key: &str, value: SettingValue) -> Result<(), SettingsError> {
        self.values.lock().unwrap().insert(key.to_owned(), value);
        Ok(())
    }
}
