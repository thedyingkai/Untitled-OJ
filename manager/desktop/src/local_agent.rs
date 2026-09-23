use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopHostPlatform {
    Windows,
    Linux,
    Macos,
    Other,
}

impl DesktopHostPlatform {
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Other
        }
    }
}

impl fmt::Display for DesktopHostPlatform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Windows => "Windows",
            Self::Linux => "Linux",
            Self::Macos => "macOS",
            Self::Other => "this platform",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopManagedExecutionUnavailableReason {
    /// Desktop cannot currently prove that private host files materialized by
    /// its process are readable by, and only by, the fixed container identity.
    WorkloadFileOwnershipContractUnverified { platform: DesktopHostPlatform },
}

impl fmt::Display for DesktopManagedExecutionUnavailableReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkloadFileOwnershipContractUnverified { platform } => write!(
                formatter,
                "managed local execution is unavailable on {platform}: Desktop has no verified host-to-container private-file ownership/ACL contract; register a standalone Agent"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopManagedExecutionCapability {
    Unavailable(DesktopManagedExecutionUnavailableReason),
}

/// Returns the managed-execution capability for the current Desktop host.
///
/// All supported Desktop platforms deliberately return `Unavailable` until a
/// platform-specific host/container ownership contract can be verified. This
/// function is the only gate for any future embedded execution implementation.
pub fn desktop_managed_execution_capability() -> DesktopManagedExecutionCapability {
    desktop_managed_execution_capability_for(DesktopHostPlatform::current())
}

pub fn desktop_managed_execution_capability_for(
    platform: DesktopHostPlatform,
) -> DesktopManagedExecutionCapability {
    DesktopManagedExecutionCapability::Unavailable(
        DesktopManagedExecutionUnavailableReason::WorkloadFileOwnershipContractUnverified {
            platform,
        },
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopAgentPhase {
    Starting,
    Running,
    Degraded,
    Unavailable,
    Stopping,
    Stopped,
    StopTimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAgentStatus {
    pub phase: DesktopAgentPhase,
    pub detail: String,
    pub retry_count: u64,
    pub unavailable_reason: Option<DesktopManagedExecutionUnavailableReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopAgentShutdown {
    pub graceful: bool,
    pub detail: String,
}

/// Compatibility handle for the disabled embedded execution component.
///
/// It owns no worker thread, transport, Docker client, lease, or publisher.
/// Keeping a handle lets the application preserve its existing bounded
/// shutdown ordering without pretending that a local execution Node exists.
pub struct DesktopAgentHandle {
    status: Arc<Mutex<DesktopAgentStatus>>,
}

impl DesktopAgentHandle {
    pub fn status(&self) -> DesktopAgentStatus {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or(DesktopAgentStatus {
                phase: DesktopAgentPhase::Degraded,
                detail: "managed local execution status lock is poisoned".to_string(),
                retry_count: 0,
                unavailable_reason: None,
            })
    }

    /// Preserves Desktop shutdown ordering. There is no execution worker to
    /// drain, so shutdown is immediate and always bounded.
    pub fn shutdown_and_join(self, _timeout: Duration) -> DesktopAgentShutdown {
        update_status(
            &self.status,
            DesktopAgentPhase::Stopping,
            "stopping managed local execution status".to_string(),
            None,
        );
        let detail =
            "managed local execution was unavailable; no worker required draining".to_string();
        update_status(
            &self.status,
            DesktopAgentPhase::Stopped,
            detail.clone(),
            None,
        );
        DesktopAgentShutdown {
            graceful: true,
            detail,
        }
    }
}

/// Creates the fail-closed lifecycle handle used by the Desktop launcher.
/// This entry point needs no control-plane bootstrap authority.
pub fn unavailable_desktop_agent() -> DesktopAgentHandle {
    let capability = desktop_managed_execution_capability();
    let DesktopManagedExecutionCapability::Unavailable(reason) = capability;
    let status = DesktopAgentStatus {
        phase: DesktopAgentPhase::Unavailable,
        detail: reason.to_string(),
        retry_count: 0,
        unavailable_reason: Some(reason),
    };
    eprintln!("OJOS Desktop: {}", status.detail);
    DesktopAgentHandle {
        status: Arc::new(Mutex::new(status)),
    }
}

fn update_status(
    status: &Arc<Mutex<DesktopAgentStatus>>,
    phase: DesktopAgentPhase,
    detail: String,
    retry_count: Option<u64>,
) {
    if let Ok(mut status) = status.lock() {
        status.phase = phase;
        status.detail = detail;
        if let Some(retry_count) = retry_count {
            status.retry_count = retry_count;
        }
    }
}
