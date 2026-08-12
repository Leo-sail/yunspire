use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager, State};

use crate::{
    runtime_db::RuntimeDatabase,
    task_runtime::RuntimeTaskStepLeaseRenewalInput,
};

/// Lease 续期配置
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 30;
const DEFAULT_LEASE_EXTENSION_SECS: u64 = 300;
const MIN_REMAINING_LEASE_SECS: u64 = 60;

/// Lease 心跳管理器
pub struct LeaseHeartbeatManager {
    running: Arc<AtomicBool>,
    worker_id: String,
}

impl LeaseHeartbeatManager {
    pub fn new(worker_id: String) -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            worker_id,
        }
    }

    /// 启动心跳守护线程
    pub fn start(&self, app: AppHandle) {
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            log::warn!("Lease 心跳守护线程已在运行");
            return;
        }

        let running = Arc::clone(&self.running);
        let worker_id = self.worker_id.clone();
        let interval = Duration::from_secs(DEFAULT_HEARTBEAT_INTERVAL_SECS);
        let extension = Duration::from_secs(DEFAULT_LEASE_EXTENSION_SECS);

        thread::spawn(move || {
            log::info!("Lease 心跳守护线程已启动，间隔 {} 秒", interval.as_secs());

            while running.load(Ordering::Acquire) {
                thread::sleep(interval);

                if !running.load(Ordering::Acquire) {
                    break;
                }

                match renew_expiring_leases(&app, &worker_id, extension) {
                    Ok(renewed) => {
                        if renewed > 0 {
                            log::info!("成功续期 {} 个 lease", renewed);
                        }
                    }
                    Err(e) => {
                        log::warn!("Lease 续期失败: {}", e);
                    }
                }
            }

            log::info!("Lease 心跳守护线程已停止");
        });
    }

    /// 停止心跳守护线程
    pub fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    /// 检查是否正在运行
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
}

impl Drop for LeaseHeartbeatManager {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 续期即将过期的 leases
fn renew_expiring_leases(
    app: &AppHandle,
    worker_id: &str,
    extension: Duration,
) -> Result<usize, String> {
    let database = app.state::<RuntimeDatabase>();
    let workspace_scope = database.local_workspace_scope()?;

    // 查询所有活动的 leases
    let active_leases = database.get_active_step_claims(&workspace_scope, worker_id)?;

    let now = SystemTime::now();
    let min_remaining = Duration::from_secs(MIN_REMAINING_LEASE_SECS);

    let mut renewed_count = 0;

    for lease in active_leases {
        // 检查是否需要续期
        let expires_at = parse_rfc3339_timestamp(&lease.lease_expires_at)?;
        let remaining = expires_at
            .duration_since(now)
            .unwrap_or(Duration::ZERO);

        if remaining < min_remaining {
            // 需要续期
            let input = RuntimeTaskStepLeaseRenewalInput {
                task_id: lease.runtime_task_id.clone(),
                step_claim_id: lease.claim_id.clone(),
                worker_id: worker_id.to_string(),
                lease_seconds: extension.as_secs(),
            };

            match database.renew_runtime_task_step_lease(&workspace_scope, &input) {
                Ok(receipt) => {
                    log::debug!(
                        "已续期 task={} step={} 新过期时间={}",
                        lease.runtime_task_id,
                        lease.step_id,
                        receipt.lease_expires_at
                    );
                    renewed_count += 1;
                }
                Err(e) => {
                    log::warn!(
                        "续期失败 task={} step={}: {}",
                        lease.runtime_task_id,
                        lease.step_id,
                        e
                    );
                }
            }
        }
    }

    Ok(renewed_count)
}

/// 解析 RFC3339 时间戳
fn parse_rfc3339_timestamp(s: &str) -> Result<SystemTime, String> {
    use chrono::{DateTime, Utc};

    let dt = DateTime::parse_from_rfc3339(s)
        .map_err(|e| format!("无效的时间戳格式 {}: {}", s, e))?
        .with_timezone(&Utc);

    let timestamp = dt.timestamp();
    if timestamp < 0 {
        return Err(format!("时间戳为负数: {}", s));
    }

    Ok(UNIX_EPOCH + Duration::from_secs(timestamp as u64))
}

/// Lease 心跳状态（全局单例）
#[derive(Default)]
pub struct LeaseHeartbeatState {
    manager: std::sync::Mutex<Option<LeaseHeartbeatManager>>,
}

impl LeaseHeartbeatState {
    /// 初始化并启动心跳
    pub fn initialize(&self, app: AppHandle, worker_id: String) -> Result<(), String> {
        let mut manager_lock = self.manager.lock()
            .map_err(|_| "无法获取心跳管理器锁".to_string())?;

        if manager_lock.is_some() {
            log::warn!("Lease 心跳管理器已初始化");
            return Ok(());
        }

        let manager = LeaseHeartbeatManager::new(worker_id);
        manager.start(app);
        *manager_lock = Some(manager);

        log::info!("Lease 心跳管理器已初始化");
        Ok(())
    }

    /// 停止心跳
    pub fn shutdown(&self) {
        if let Ok(mut manager_lock) = self.manager.lock() {
            if let Some(manager) = manager_lock.take() {
                manager.stop();
                log::info!("Lease 心跳管理器已停止");
            }
        }
    }

    /// 检查是否正在运行
    pub fn is_running(&self) -> bool {
        self.manager
            .lock()
            .ok()
            .and_then(|lock| lock.as_ref().map(|m| m.is_running()))
            .unwrap_or(false)
    }
}

/// Tauri 命令：获取心跳状态
#[tauri::command]
pub fn get_lease_heartbeat_status(
    state: State<'_, LeaseHeartbeatState>,
) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "running": state.is_running(),
        "interval_seconds": DEFAULT_HEARTBEAT_INTERVAL_SECS,
        "lease_extension_seconds": DEFAULT_LEASE_EXTENSION_SECS,
        "min_remaining_seconds": MIN_REMAINING_LEASE_SECS,
    }))
}

/// 启动 lease 心跳（在应用初始化时调用）
pub(crate) fn start_lease_heartbeat_if_needed(
    app: &AppHandle,
    worker_id: String,
) -> Result<(), String> {
    let state = app.state::<LeaseHeartbeatState>();
    state.initialize(app.clone(), worker_id)
}
