//! 测试进程环境隔离。
//!
//! Rust 测试默认并行运行，而进程环境是全局共享状态。所有需要读取后再修改环境变量
//! 的后端测试都必须经过这里，避免一个测试临时设置令牌时让其他路由测试误判为未授权。

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

static PROCESS_ENV_LOCK: Mutex<()> = Mutex::new(());

/// 持锁期间可以安全修改进程环境；离开作用域时恢复每个变量最初的值。
pub(crate) struct TestEnv {
    _lock: MutexGuard<'static, ()>,
    originals: BTreeMap<String, Option<OsString>>,
}

impl TestEnv {
    pub(crate) fn lock() -> Self {
        let lock = PROCESS_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self {
            _lock: lock,
            originals: BTreeMap::new(),
        }
    }

    pub(crate) fn set(&mut self, name: &str, value: &str) {
        self.remember(name);
        // SAFETY: every backend test that mutates process environment holds PROCESS_ENV_LOCK.
        unsafe { std::env::set_var(name, value) };
    }

    pub(crate) fn remove(&mut self, name: &str) {
        self.remember(name);
        // SAFETY: every backend test that mutates process environment holds PROCESS_ENV_LOCK.
        unsafe { std::env::remove_var(name) };
    }

    pub(crate) fn apply(&mut self, name: &str, value: Option<&str>) {
        match value {
            Some(value) => self.set(name, value),
            None => self.remove(name),
        }
    }

    fn remember(&mut self, name: &str) {
        self.originals
            .entry(name.to_string())
            .or_insert_with(|| std::env::var_os(name));
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        for (name, value) in &self.originals {
            // SAFETY: this guard still owns PROCESS_ENV_LOCK while restoring the snapshot.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}
