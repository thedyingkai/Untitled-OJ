//! Web UI 静态文件托管：daemon 直接对外提供编排器前端。

use std::fs;
use std::path::{Component, Path, PathBuf};

pub struct StaticResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub cache_control: &'static str,
    pub body: Vec<u8>,
    pub content_length: Option<usize>,
}

fn content_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn sanitized_relative(path: &str) -> Option<PathBuf> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Some(PathBuf::from("index.html"));
    }
    let relative = Path::new(trimmed);
    let mut sanitized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => sanitized.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if sanitized.as_os_str().is_empty() {
        Some(PathBuf::from("index.html"))
    } else {
        Some(sanitized)
    }
}

/// 解析真实路径并确认它仍在 web_root 之内。`sanitized_relative` 只挡住了 `..` 这类
/// 路径成分，挡不住 web_root 内部指向外面的 symlink（或 Windows 的目录联接），
/// 因此读文件之前必须再 canonicalize 一次做前缀校验。
fn resolve_inside_root(root: &Path, candidate: &Path) -> Option<PathBuf> {
    let resolved = fs::canonicalize(candidate).ok()?;
    if resolved.starts_with(root) {
        Some(resolved)
    } else {
        None
    }
}

/// 尝试从 web_root 提供静态文件。返回 None 表示该请求不该由静态层处理
/// （文件不存在、逃出了 web_root，或不适合回退 index.html）。
pub fn try_serve(web_root: &Path, method: &str, path: &str) -> Option<StaticResponse> {
    if method != "GET" {
        return None;
    }
    let relative = sanitized_relative(path)?;
    // web_root 本身先规范化，之后所有前缀比较都在规范化路径之间进行。
    let root = fs::canonicalize(web_root).ok()?;
    let full = root.join(&relative);
    if full.is_file() {
        let resolved = resolve_inside_root(&root, &full)?;
        let body = fs::read(&resolved).ok()?;
        let hashed_asset = relative
            .components()
            .next()
            .map(|component| component.as_os_str() == "assets")
            .unwrap_or(false);
        return Some(StaticResponse {
            status: 200,
            content_type: content_type_for(&full),
            cache_control: if hashed_asset {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            },
            body,
            content_length: None,
        });
    }
    // SPA 回退：无扩展名的 GET 请求返回 index.html（哈希路由下仅根路径需要，
    // 但兼容 history 模式路径）。
    let has_extension = relative.extension().is_some();
    if !has_extension {
        let index = root.join("index.html");
        if index.is_file() {
            let resolved = resolve_inside_root(&root, &index)?;
            let body = fs::read(&resolved).ok()?;
            return Some(StaticResponse {
                status: 200,
                content_type: "text/html; charset=utf-8",
                cache_control: "no-cache",
                body,
                content_length: None,
            });
        }
    }
    None
}

/// web_root 缺失时根路径的引导页，避免 404 造成困惑。
pub fn placeholder_page() -> StaticResponse {
    let body = r#"<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>OJOS Orchestrator</title>
<style>body{font-family:system-ui,sans-serif;background:#0b0f17;color:#e5e7eb;display:flex;align-items:center;justify-content:center;height:100vh;margin:0}
main{max-width:560px;padding:32px;background:#111827;border:1px solid rgba(255,255,255,.08);border-radius:12px}
code{background:#1f2937;padding:2px 6px;border-radius:4px}</style></head>
<body><main><h1>OJOS Orchestrator daemon 运行中</h1>
<p>Web UI 构建产物未找到。请先构建前端：</p>
<p><code>cd manager/web &amp;&amp; npm ci &amp;&amp; npm run build</code></p>
<p>或使用 <code>--web-root</code> 指定构建产物目录。API 仍可正常使用，例如 <a href="/health" style="color:#818cf8">/health</a>。</p>
</main></body></html>"#
        .as_bytes()
        .to_vec();
    StaticResponse {
        status: 200,
        content_type: "text/html; charset=utf-8",
        cache_control: "no-cache",
        body,
        content_length: None,
    }
}
