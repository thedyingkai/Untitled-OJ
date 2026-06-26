use anyhow::{Result, anyhow};

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use anyhow::Context;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    #[derive(Debug)]
    pub struct CgroupRun {
        path: Option<PathBuf>,
    }

    impl CgroupRun {
        pub fn create(memory_mb: u64, pids_max: u64) -> Result<Self> {
            let root = detect_cgroup_v2_root()?;

            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock before unix epoch")?
                .as_nanos();
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = root.join("ojos").join("judge-worker").join(format!(
                "{}-{}-{}",
                std::process::id(),
                now,
                id
            ));

            if let Err(err) = create_limited_cgroup(&path, memory_mb, pids_max) {
                if allow_cgroup_fallback() {
                    let _ = std::fs::remove_dir(&path);
                    return Ok(Self { path: None });
                }
                return Err(err);
            }

            Ok(Self { path: Some(path) })
        }

        pub fn attach(&self, pid: u32) -> Result<()> {
            let Some(path) = &self.path else {
                return Ok(());
            };
            std::fs::write(path.join("cgroup.procs"), pid.to_string())
                .with_context(|| format!("attach pid to cgroup failed: {}", path.display()))
        }

        pub fn memory_peak_kb(&self) -> Result<i32> {
            let Some(cgroup_path) = &self.path else {
                return Ok(0);
            };
            let path = cgroup_path.join("memory.peak");
            let value = if path.exists() {
                read_u64(&path)?
            } else {
                read_u64(&cgroup_path.join("memory.current"))?
            };
            Ok((value / 1024).min(i32::MAX as u64) as i32)
        }

        pub fn oom_killed(&self) -> Result<bool> {
            let Some(path) = &self.path else {
                return Ok(false);
            };
            let events = std::fs::read_to_string(path.join("memory.events"))
                .with_context(|| format!("read memory.events failed: {}", path.display()))?;
            for line in events.lines() {
                let mut parts = line.split_whitespace();
                let key = parts.next().unwrap_or_default();
                let value = parts
                    .next()
                    .and_then(|raw| raw.parse::<u64>().ok())
                    .unwrap_or(0);
                if matches!(key, "oom" | "oom_kill" | "oom_group_kill") && value > 0 {
                    return Ok(true);
                }
            }
            Ok(false)
        }
    }

    impl Drop for CgroupRun {
        fn drop(&mut self) {
            if let Some(path) = &self.path {
                let _ = std::fs::remove_dir(path);
            }
        }
    }

    fn create_limited_cgroup(path: &Path, memory_mb: u64, pids_max: u64) -> Result<()> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("create cgroup failed: {}", path.display()))?;

        std::fs::write(
            path.join("memory.max"),
            (memory_mb * 1024 * 1024).to_string(),
        )
        .with_context(|| format!("write memory.max failed: {}", path.display()))?;
        std::fs::write(path.join("pids.max"), pids_max.to_string())
            .with_context(|| format!("write pids.max failed: {}", path.display()))?;

        Ok(())
    }

    fn allow_cgroup_fallback() -> bool {
        std::env::var("OJOS_ALLOW_CGROUP_FALLBACK")
            .map(|value| {
                let value = value.trim();
                value == "1"
                    || value.eq_ignore_ascii_case("true")
                    || value.eq_ignore_ascii_case("yes")
            })
            .unwrap_or(false)
    }

    fn read_u64(path: &Path) -> Result<u64> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read cgroup file failed: {}", path.display()))?;
        text.trim()
            .parse::<u64>()
            .with_context(|| format!("parse cgroup value failed: {}", path.display()))
    }

    fn detect_cgroup_v2_root() -> Result<PathBuf> {
        let mut candidates = Vec::new();
        if let Ok(raw) = std::env::var("OJOS_CGROUP_V2_ROOT") {
            if !raw.trim().is_empty() {
                candidates.push(PathBuf::from(raw));
            }
        }
        candidates.push(PathBuf::from("/sys/fs/cgroup"));
        candidates.push(PathBuf::from("/sys/fs/cgroup/unified"));

        for root in candidates {
            if root.join("cgroup.controllers").exists() {
                return Ok(root);
            }
        }

        Err(anyhow!(
            "cgroup v2 is required: checked OJOS_CGROUP_V2_ROOT, /sys/fs/cgroup, /sys/fs/cgroup/unified"
        ))
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;

    #[derive(Debug)]
    pub struct CgroupRun;

    impl CgroupRun {
        pub fn create(_memory_mb: u64, _pids_max: u64) -> Result<Self> {
            Err(anyhow!("cgroup v2 memory enforcement requires Linux"))
        }

        pub fn attach(&self, _pid: u32) -> Result<()> {
            Ok(())
        }

        pub fn memory_peak_kb(&self) -> Result<i32> {
            Ok(0)
        }

        pub fn oom_killed(&self) -> Result<bool> {
            Ok(false)
        }
    }
}

pub use imp::CgroupRun;
