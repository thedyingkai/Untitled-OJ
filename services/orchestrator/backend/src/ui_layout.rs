//! 拓扑画布布局持久化：节点坐标等 UI 状态，存于 repo 根 .ojos/ui-layout.json。

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const MAX_LAYOUT_BYTES: usize = 512 * 1024;

/// 进程内串行化布局读写：并发 PUT 之间不会互相覆盖临时文件，GET 也不会读到写了一半的内容。
static LAYOUT_LOCK: Mutex<()> = Mutex::new(());

fn layout_guard() -> MutexGuard<'static, ()> {
    // 布局文件是纯 UI 状态，锁中毒时降级继续用：不值得让画布整体不可用。
    LAYOUT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn layout_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".ojos").join("ui-layout.json")
}

/// 读取布局。文件损坏/不可读时降级为空布局而不是报错，避免一次坏写让画布再也打不开。
pub fn get_layout(repo_root: &Path) -> Result<Value> {
    let _guard = layout_guard();
    let path = layout_path(repo_root);
    if !path.is_file() {
        return Ok(json!({ "layout": {} }));
    }
    let layout = match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                eprintln!(
                    "ui layout {} is not a JSON object; falling back to an empty layout",
                    path.display()
                );
                json!({})
            }
            Err(err) => {
                eprintln!(
                    "parse ui layout {} failed: {err}; falling back to an empty layout",
                    path.display()
                );
                json!({})
            }
        },
        Err(err) => {
            eprintln!(
                "read ui layout {} failed: {err}; falling back to an empty layout",
                path.display()
            );
            json!({})
        }
    };
    Ok(json!({ "layout": layout }))
}

/// 写入布局：先落临时文件再 rename 提交，保证读者要么看到旧内容要么看到新内容，
/// 不会看到截断的半个 JSON。
pub fn put_layout(repo_root: &Path, body: &str) -> Result<Value> {
    if body.len() > MAX_LAYOUT_BYTES {
        return Err(anyhow!("ui layout exceeds {MAX_LAYOUT_BYTES} bytes"));
    }
    let layout: Value = serde_json::from_str(body.trim())
        .map_err(|err| anyhow!("ui layout must be valid JSON: {err}"))?;
    if !layout.is_object() {
        return Err(anyhow!("ui layout must be a JSON object"));
    }
    let text = serde_json::to_string_pretty(&layout)?;

    let _guard = layout_guard();
    let path = layout_path(repo_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| anyhow!("create {} failed: {err}", parent.display()))?;
    }
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, text)
        .map_err(|err| anyhow!("write ui layout {} failed: {err}", temp.display()))?;
    if let Err(err) = fs::rename(&temp, &path) {
        let _ = fs::remove_file(&temp);
        return Err(anyhow!("commit ui layout {} failed: {err}", path.display()));
    }
    Ok(json!({ "layout": layout, "saved": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_layout_commits_atomically_and_leaves_no_temp_file() {
        let root = std::env::temp_dir().join(format!(
            "ojos-ui-layout-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("layout test root");

        let saved = put_layout(&root, r#"{"nodes":{"a":{"x":1,"y":2}}}"#).expect("put layout");
        assert_eq!(saved["saved"], json!(true));
        let path = layout_path(&root);
        assert!(path.is_file());
        assert!(!path.with_extension("json.tmp").exists());

        let loaded = get_layout(&root).expect("get layout");
        assert_eq!(loaded["layout"]["nodes"]["a"]["x"], json!(1));

        // 坏文件降级成空布局而不是报错。
        fs::write(&path, "{ this is not json").expect("corrupt layout");
        let degraded = get_layout(&root).expect("degraded layout");
        assert_eq!(degraded["layout"], json!({}));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn put_layout_rejects_non_object_and_oversized_bodies() {
        let root =
            std::env::temp_dir().join(format!("ojos-ui-layout-reject-{}", std::process::id()));
        assert!(put_layout(&root, "[]").is_err());
        assert!(put_layout(&root, "not json").is_err());
        let oversized = format!("{{\"pad\":\"{}\"}}", "x".repeat(MAX_LAYOUT_BYTES));
        assert!(put_layout(&root, &oversized).is_err());
    }
}
