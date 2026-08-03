use orchestrator_legacy::{
    ActionDispatchResult, ActionRequest, OperationWorkbenchContext, OrchestratorActionConsole,
    OrchestratorView,
};
use orchestrator_manager::{
    GithubReleaseListView, InstalledServiceView, StoreCatalog, StoreIndexView, StoreInstallRequest,
    StoreInstallResult, installed_services, installed_services_from_deployments,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum WorkPurpose {
    StoreUninstall,
    RuntimeAction,
}

pub enum ManagerTask {
    Refresh,
    LoadStoreIndex {
        refresh: bool,
    },
    LoadGithubReleases {
        repo: String,
    },
    Install(StoreInstallRequest),
    Dispatch {
        request: ActionRequest,
        purpose: WorkPurpose,
    },
    Stop,
}

#[derive(Debug)]
pub struct CoreSnapshot {
    pub context: OperationWorkbenchContext,
    pub view: OrchestratorView,
    pub installed: BTreeMap<String, InstalledServiceView>,
}

#[derive(Debug)]
pub struct ActionCompletion {
    pub result: ActionDispatchResult,
    pub snapshot: CoreSnapshot,
}

#[derive(Debug)]
pub struct InstallCompletion {
    pub result: StoreInstallResult,
    pub snapshot: CoreSnapshot,
}

#[derive(Debug)]
pub enum ManagerEvent {
    Refreshed(Result<Box<CoreSnapshot>, String>),
    StoreIndexLoaded(Result<Box<StoreIndexView>, String>),
    GithubReleasesLoaded(Result<Box<GithubReleaseListView>, String>),
    Installed(Result<Box<InstallCompletion>, String>),
    Dispatched {
        purpose: WorkPurpose,
        completion: Result<Box<ActionCompletion>, String>,
    },
}

pub struct ManagerWorker {
    sender: mpsc::Sender<ManagerTask>,
    receiver: mpsc::Receiver<ManagerEvent>,
    pending: usize,
    join: Option<thread::JoinHandle<()>>,
}

impl ManagerWorker {
    pub fn spawn(
        console: Arc<Mutex<OrchestratorActionConsole>>,
        catalog: Arc<StoreCatalog>,
        repo_root: PathBuf,
    ) -> Self {
        let (task_sender, task_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let join = thread::spawn(move || {
            while let Ok(task) = task_receiver.recv() {
                let event = match task {
                    ManagerTask::Refresh => ManagerEvent::Refreshed(
                        with_console(&console, |console: &mut OrchestratorActionConsole| {
                            snapshot(console)
                        })
                        .map(Box::new),
                    ),
                    ManagerTask::LoadStoreIndex { refresh } => {
                        let loaded = catalog
                            .load_index(&repo_root, refresh)
                            .map_err(|err| err.to_string())
                            .and_then(|(index_url, cached, index)| {
                                with_console(&console, |console| {
                                    Ok(StoreIndexView {
                                        index_url,
                                        cached,
                                        index,
                                        installed: installed_services(console)?,
                                    })
                                })
                            });
                        ManagerEvent::StoreIndexLoaded(loaded.map(Box::new))
                    }
                    ManagerTask::LoadGithubReleases { repo } => ManagerEvent::GithubReleasesLoaded(
                        catalog
                            .github_releases(&repo, 20)
                            .map(Box::new)
                            .map_err(|err| err.to_string()),
                    ),
                    ManagerTask::Install(request) => ManagerEvent::Installed(
                        with_console(&console, |console| {
                            let result = catalog.install(console, &repo_root, request)?;
                            let snapshot = snapshot(console)?;
                            Ok(InstallCompletion { result, snapshot })
                        })
                        .map(Box::new),
                    ),
                    ManagerTask::Dispatch { request, purpose } => ManagerEvent::Dispatched {
                        purpose,
                        completion: with_console(&console, |console| {
                            let result = console.dispatch(request)?;
                            let snapshot = snapshot(console)?;
                            Ok(ActionCompletion { result, snapshot })
                        })
                        .map(Box::new),
                    },
                    ManagerTask::Stop => break,
                };
                if event_sender.send(event).is_err() {
                    break;
                }
            }
        });
        Self {
            sender: task_sender,
            receiver: event_receiver,
            pending: 0,
            join: Some(join),
        }
    }

    pub fn submit(&mut self, task: ManagerTask) -> Result<(), String> {
        self.sender
            .send(task)
            .map_err(|_| "TUI 后台管理任务线程已停止".to_string())?;
        self.pending += 1;
        Ok(())
    }

    pub fn try_next(&mut self) -> Option<ManagerEvent> {
        let event = self.receiver.try_recv().ok()?;
        self.pending = self.pending.saturating_sub(1);
        Some(event)
    }

    #[cfg(test)]
    pub fn recv(&mut self) -> Option<ManagerEvent> {
        let event = self.receiver.recv().ok()?;
        self.pending = self.pending.saturating_sub(1);
        Some(event)
    }

    pub fn is_busy(&self) -> bool {
        self.pending > 0
    }
}

impl Drop for ManagerWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(ManagerTask::Stop);
        if self.pending == 0
            && let Some(join) = self.join.take()
        {
            let _ = join.join();
        }
    }
}

fn with_console<T>(
    console: &Arc<Mutex<OrchestratorActionConsole>>,
    callback: impl FnOnce(&mut OrchestratorActionConsole) -> anyhow::Result<T>,
) -> Result<T, String> {
    let mut console = console
        .lock()
        .map_err(|_| "TUI 编排器状态锁已损坏".to_string())?;
    callback(&mut console).map_err(|err| err.to_string())
}

fn snapshot(console: &OrchestratorActionConsole) -> anyhow::Result<CoreSnapshot> {
    let context = console.context()?;
    let view = console.view()?;
    let installed = installed_services_from_deployments(view.deployments.clone())?;
    Ok(CoreSnapshot {
        context,
        view,
        installed,
    })
}
