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
        path: PathBuf,
    }

    impl CgroupRun {
        pub fn create(memory_mb: u64, pids_max: u64) -> Result<Self> {
            let root = std::env::var("OJOS_CGROUP_V2_ROOT")
                .unwrap_or_else(|_| "/sys/fs/cgroup".to_string());
            let root = PathBuf::from(root);
            if !root.join("cgroup.controllers").exists() {
                return Err(anyhow!("cgroup v2 is required: {}", root.display()));
            }

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

            std::fs::create_dir_all(&path)
                .with_context(|| format!("create cgroup failed: {}", path.display()))?;

            std::fs::write(
                path.join("memory.max"),
                (memory_mb * 1024 * 1024).to_string(),
            )
            .with_context(|| format!("write memory.max failed: {}", path.display()))?;
            std::fs::write(path.join("pids.max"), pids_max.to_string())
                .with_context(|| format!("write pids.max failed: {}", path.display()))?;

            Ok(Self { path })
        }

        pub fn attach(&self, pid: u32) -> Result<()> {
            std::fs::write(self.path.join("cgroup.procs"), pid.to_string())
                .with_context(|| format!("attach pid to cgroup failed: {}", self.path.display()))
        }

        pub fn memory_peak_kb(&self) -> Result<i32> {
            let path = self.path.join("memory.peak");
            let value = if path.exists() {
                read_u64(&path)?
            } else {
                read_u64(&self.path.join("memory.current"))?
            };
            Ok((value / 1024).min(i32::MAX as u64) as i32)
        }

        pub fn oom_killed(&self) -> Result<bool> {
            let events = std::fs::read_to_string(self.path.join("memory.events"))
                .with_context(|| format!("read memory.events failed: {}", self.path.display()))?;
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
            let _ = std::fs::remove_dir(&self.path);
        }
    }

    fn read_u64(path: &Path) -> Result<u64> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read cgroup file failed: {}", path.display()))?;
        text.trim()
            .parse::<u64>()
            .with_context(|| format!("parse cgroup value failed: {}", path.display()))
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
