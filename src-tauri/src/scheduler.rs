use crate::runtime_db::RuntimeDatabase;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter, Manager};

const SCHEDULER_POLL_SECONDS: u64 = 15;

#[derive(Default)]
pub struct SchedulerState {
    started: AtomicBool,
    authorized: AtomicBool,
}

impl SchedulerState {
    fn set_authorized(&self, authorized: bool) {
        self.authorized.store(authorized, Ordering::Release);
    }

    fn is_authorized(&self) -> bool {
        self.authorized.load(Ordering::Acquire)
    }
}

pub fn start_scheduler(app: &AppHandle) {
    let state = app.state::<SchedulerState>();
    state.set_authorized(true);
    if state.started.swap(true, Ordering::AcqRel) {
        return;
    }
    let app_handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(SCHEDULER_POLL_SECONDS));
        loop {
            interval.tick().await;
            if !app_handle.state::<SchedulerState>().is_authorized() {
                continue;
            }
            let database = app_handle.state::<RuntimeDatabase>();
            let workspace_scope = match database.local_workspace_scope() {
                Ok(scope) => scope,
                Err(error) => {
                    log::warn!("无法读取本地工作区：{error}");
                    continue;
                }
            };
            match database.claim_due_runtime_schedules(&workspace_scope, 32) {
                Ok(due) => {
                    for schedule in due {
                        if let Err(error) = app_handle.emit("yunspire://schedule-due", schedule) {
                            log::warn!("无法发送原生日程唤醒事件：{error}");
                        }
                    }
                }
                Err(error) => log::warn!("原生调度器轮询失败：{error}"),
            }
        }
    });
}

pub fn pause_scheduler(app: &AppHandle) {
    app.state::<SchedulerState>().set_authorized(false);
}
