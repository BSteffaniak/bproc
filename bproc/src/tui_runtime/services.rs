//! Single-owner serialized side effects. Requests are admitted synchronously in update order;
//! BMUX commands only await receipts and never own/cancel a kernel mutation.
use super::*;
use crate::backends::ActiveThrottle;
use crate::process::{ThrottleLimit, TrafficType};
use std::collections::HashMap;

#[derive(Clone)]
pub(super) struct ThrottleRequest {
    pub pid: i32,
    pub name: String,
    pub limit: ThrottleLimit,
}
pub(super) enum Effect {
    Apply(ThrottleRequest),
    Resolve {
        request: ThrottleRequest,
        is_upload: bool,
        action: ui::BackendCompatibilityAction,
    },
    Remove {
        pid: i32,
        name: String,
    },
    SelectBackend {
        group: ui::BackendGroup,
        name: String,
    },
    Preferences {
        filters: Option<Vec<String>>,
        traffic: Option<TrafficType>,
    },
    Refresh,
    Shutdown {
        filters: Option<Vec<String>>,
        traffic: Option<TrafficType>,
    },
}
pub(super) struct ServiceResult {
    pub sequence: u64,
    pub status: Option<String>,
    pub dialog: Option<ui::BackendCompatibilityDialog>,
    pub close_throttle: bool,
    pub backend: BackendInfo,
    pub throttles: HashMap<i32, ActiveThrottle>,
}
struct Request {
    sequence: u64,
    effect: Effect,
    reply: tokio::sync::oneshot::Sender<ServiceResult>,
}
pub(super) struct Service {
    tx: std::sync::mpsc::SyncSender<Request>,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
    sequence: u64,
}
impl Service {
    pub fn start(
        manager: ThrottleManager,
        config: config::Config,
        monitor: tokio::sync::mpsc::UnboundedSender<MonitorCommand>,
        save: bool,
        socket_mapper: Option<String>,
    ) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Request>(32);
        let thread = std::thread::spawn(move || {
            let mut owner = Owner {
                manager,
                config,
                monitor,
                save,
                pending_resolution: None,
                socket_mapper,
                socket_mappers: crate::backends::process::socket_mapper::detect_socket_mappers()
                    .into_iter()
                    .map(|m| (m.name.to_string(), m.priority, m.available))
                    .collect(),
            };
            for request in rx {
                let shutdown = matches!(request.effect, Effect::Shutdown { .. });
                let result = owner.apply(request.sequence, request.effect);
                let _ = request.reply.send(result);
                if shutdown {
                    return owner.manager.cleanup();
                }
            }
            owner.manager.cleanup()
        });
        Self {
            tx,
            thread: Some(thread),
            sequence: 0,
        }
    }
    pub fn request(
        &mut self,
        effect: Effect,
    ) -> Result<tokio::sync::oneshot::Receiver<ServiceResult>> {
        let sequence = self.sequence + 1;
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.tx
            .try_send(Request {
                sequence,
                effect,
                reply,
            })
            .map_err(|error| match error {
                std::sync::mpsc::TrySendError::Full(_) => anyhow::anyhow!(
                    "service busy; action not accepted, retry after pending work completes"
                ),
                std::sync::mpsc::TrySendError::Disconnected(_) => {
                    anyhow::anyhow!("effect service stopped")
                }
            })?;
        self.sequence = sequence;
        Ok(rx)
    }
    // Shutdown is outside Program::update and must wait for FIFO admission, not reject a full queue.
    pub async fn finish(&mut self, effect: Effect) -> Result<ServiceResult> {
        self.sequence += 1;
        let (reply, rx) = tokio::sync::oneshot::channel();
        let tx = self.tx.clone();
        let sequence = self.sequence;
        tokio::task::spawn_blocking(move || {
            tx.send(Request {
                sequence,
                effect,
                reply,
            })
            .map_err(|_| anyhow::anyhow!("effect service stopped"))
        })
        .await??;
        Ok(rx.await?)
    }
    pub fn join(&mut self) -> Result<()> {
        self.thread
            .take()
            .expect("service joined once")
            .join()
            .map_err(|_| anyhow::anyhow!("effect service panicked"))?
    }
}
struct Owner {
    manager: ThrottleManager,
    config: config::Config,
    monitor: tokio::sync::mpsc::UnboundedSender<MonitorCommand>,
    save: bool,
    pending_resolution: Option<(ThrottleRequest, Option<String>, Option<String>)>,
    socket_mapper: Option<String>,
    socket_mappers: Vec<(String, crate::backends::BackendPriority, bool)>,
}
impl Owner {
    fn persist(&self) -> Result<()> {
        if self.save {
            self.config.save()?;
        }
        Ok(())
    }
    fn backend(&self) -> BackendInfo {
        let mut info = self.manager.get_backend_info(
            self.config.preferred_upload_backend.clone(),
            self.config.preferred_download_backend.clone(),
        );
        info.available_socket_mappers = self.socket_mappers.clone();
        info.active_socket_mapper = self.socket_mapper.clone();
        info.preferred_socket_mapper = self.config.preferred_socket_mapper.clone();
        info.active_monitoring = Some("pnet".into());
        info
    }
    fn compatibility(&self, request: &ThrottleRequest) -> Option<ui::BackendCompatibilityDialog> {
        let traffic = request.limit.traffic_type;
        let (upload, download) = self.manager.get_default_backends();
        if request.limit.upload_limit.is_some()
            && !self.manager.current_upload_backend_supports(traffic)
        {
            Some(ui::BackendCompatibilityDialog::new(
                upload.unwrap_or_else(|| "none".into()),
                traffic,
                self.manager.find_compatible_upload_backends(traffic),
                true,
            ))
        } else if request.limit.download_limit.is_some()
            && !self.manager.current_download_backend_supports(traffic)
        {
            Some(ui::BackendCompatibilityDialog::new(
                download.unwrap_or_else(|| "none".into()),
                traffic,
                self.manager.find_compatible_download_backends(traffic),
                false,
            ))
        } else {
            None
        }
    }
    fn apply(&mut self, sequence: u64, effect: Effect) -> ServiceResult {
        let mut dialog = None;
        let mut close_throttle = false;
        let result: Result<Option<String>> = (|| {
            match effect {
                Effect::Apply(request) => {
                    self.pending_resolution = None;
                    dialog = self.compatibility(&request);
                    if dialog.is_some() {
                        return Ok(None);
                    }
                    close_throttle = true;
                    self.manager.throttle_process(
                        request.pid,
                        request.name.clone(),
                        &request.limit,
                    )?;
                    Ok(Some(format!(
                        "Throttle applied to {} (PID {})",
                        request.name, request.pid
                    )))
                }
                Effect::Resolve {
                    mut request,
                    is_upload,
                    action,
                } => {
                    let original = self.manager.get_default_backends();
                    let pending = self.pending_resolution.take().filter(|(old, _, _)| {
                        old.pid == request.pid
                            && old.name == request.name
                            && old.limit.upload_limit == request.limit.upload_limit
                            && old.limit.download_limit == request.limit.download_limit
                            && old.limit.traffic_type == request.limit.traffic_type
                    });
                    let (_, mut temporary_upload, mut temporary_download) =
                        pending.unwrap_or_else(|| (request.clone(), None, None));
                    let resolution: Result<Option<String>> = (|| {
                        if let Some(name) = &temporary_upload {
                            self.manager.set_default_upload_backend(name)?;
                        }
                        if let Some(name) = &temporary_download {
                            self.manager.set_default_download_backend(name)?;
                        }
                        match &action {
                            ui::BackendCompatibilityAction::Cancel => return Ok(None),
                            ui::BackendCompatibilityAction::ConvertToAll => {
                                request.limit.traffic_type = TrafficType::All
                            }
                            ui::BackendCompatibilityAction::SwitchTemporary(name)
                            | ui::BackendCompatibilityAction::SwitchAndMakeDefault(name) => {
                                if is_upload {
                                    self.manager.set_default_upload_backend(name)?;
                                    temporary_upload = Some(name.clone());
                                } else {
                                    self.manager.set_default_download_backend(name)?;
                                    temporary_download = Some(name.clone());
                                }
                            }
                        }
                        // A compatible upload choice does not imply download compatibility (or vice versa).
                        dialog = self.compatibility(&request);
                        if dialog.is_some() {
                            self.pending_resolution = Some((
                                request.clone(),
                                temporary_upload.clone(),
                                temporary_download.clone(),
                            ));
                            return Ok(None);
                        }
                        self.manager.throttle_process(
                            request.pid,
                            request.name.clone(),
                            &request.limit,
                        )?;
                        close_throttle = true;
                        Ok(Some(format!(
                            "Throttle applied to {} (PID {})",
                            request.name, request.pid
                        )))
                    })();
                    // Roll back exact defaults on every path, including None and failed apply/check.
                    self.manager.restore_default_backends(original);
                    let status = resolution?;
                    if let ui::BackendCompatibilityAction::SwitchAndMakeDefault(name) = action {
                        if is_upload {
                            self.manager.set_default_upload_backend(&name)?;
                            self.config.preferred_upload_backend = Some(name);
                        } else {
                            self.manager.set_default_download_backend(&name)?;
                            self.config.preferred_download_backend = Some(name);
                        }
                        self.persist()?;
                    }
                    Ok(status)
                }
                Effect::Remove { pid, name } => {
                    self.manager.remove_throttle(pid)?;
                    Ok(Some(format!(
                        "Throttle removed from {} (PID {})",
                        name, pid
                    )))
                }
                Effect::SelectBackend { group, name } => {
                    match group {
                        ui::BackendGroup::Upload => {
                            self.manager.set_default_upload_backend(&name)?;
                            self.config.preferred_upload_backend = Some(name.clone());
                        }
                        ui::BackendGroup::Download => {
                            self.manager.set_default_download_backend(&name)?;
                            self.config.preferred_download_backend = Some(name.clone());
                        }
                        ui::BackendGroup::SocketMapper => {
                            let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
                            self.monitor
                                .send(MonitorCommand::SwitchSocketMapper {
                                    backend_name: name.clone(),
                                    response_tx,
                                })
                                .map_err(|_| anyhow::anyhow!("monitor command channel closed"))?;
                            let deadline =
                                std::time::Instant::now() + std::time::Duration::from_secs(5);
                            loop {
                                match response_rx.try_recv() {
                                    Ok(result) => {
                                        result?;
                                        break;
                                    }
                                    Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                                        anyhow::bail!("monitor dropped switch response")
                                    }
                                    Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                                        if std::time::Instant::now() >= deadline {
                                            anyhow::bail!(
                                                "socket mapper switch timed out; monitor state will reconcile on next update"
                                            );
                                        }
                                        std::thread::sleep(std::time::Duration::from_millis(10));
                                    }
                                }
                            }
                            self.socket_mapper = Some(name.clone());
                            self.config.preferred_socket_mapper = Some(name.clone());
                        }
                    }
                    self.persist()?;
                    Ok(Some(format!("Backend → {}", name)))
                }
                Effect::Preferences { filters, traffic } => {
                    self.config.filtered_interfaces = filters;
                    self.config.traffic_view_mode = traffic;
                    self.persist()?;
                    Ok(None)
                }
                Effect::Refresh => Ok(None),
                Effect::Shutdown { filters, traffic } => {
                    self.config.filtered_interfaces = filters;
                    self.config.traffic_view_mode = traffic;
                    if self.save {
                        self.config.clear_throttles();
                        for (pid, throttle) in self.manager.get_all_throttles() {
                            self.config.set_throttle(
                                pid,
                                config::SavedThrottle {
                                    process_name: throttle.process_name,
                                    upload_limit: throttle.upload_limit,
                                    download_limit: throttle.download_limit,
                                },
                            );
                        }
                        self.persist()?;
                    }
                    Ok(None)
                }
            }
        })();
        ServiceResult {
            sequence,
            status: match result {
                Ok(status) => status,
                Err(error) => Some(format!("Failed: {error:#}")),
            },
            dialog,
            close_throttle,
            backend: self.backend(),
            throttles: self.manager.get_all_throttles(),
        }
    }
}

#[cfg(test)]
impl Service {
    pub(super) fn disconnected_for_test() -> Self {
        let (tx, _rx) = std::sync::mpsc::sync_channel(1);
        Self {
            tx,
            thread: None,
            sequence: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_admission_does_not_block_or_consume_sequence() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let mut service = Service {
            tx,
            thread: None,
            sequence: 0,
        };
        let first = service.request(Effect::Refresh).unwrap();
        assert!(service.request(Effect::Refresh).is_err());
        assert_eq!(service.sequence, 1);
        assert_eq!(rx.recv().unwrap().sequence, 1);
        let second = service.request(Effect::Refresh).unwrap();
        assert_eq!(rx.recv().unwrap().sequence, 2);
        drop((first, second));
    }

    #[tokio::test]
    async fn full_queue_shutdown_waits_for_space_and_preserves_fifo() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let mut service = Service {
            tx,
            thread: None,
            sequence: 0,
        };
        let first = service.request(Effect::Refresh).unwrap();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            assert!(matches!(rx.recv().unwrap().effect, Effect::Refresh));
            let shutdown = rx.recv().unwrap();
            assert_eq!(shutdown.sequence, 2);
            assert!(matches!(shutdown.effect, Effect::Shutdown { .. }));
            let _ = shutdown.reply.send(ServiceResult {
                sequence: 2,
                status: None,
                dialog: None,
                close_throttle: false,
                backend: BackendInfo {
                    active_upload: None,
                    active_download: None,
                    active_monitoring: None,
                    active_socket_mapper: None,
                    available_upload: vec![],
                    available_download: vec![],
                    available_socket_mappers: vec![],
                    preferred_upload: None,
                    preferred_download: None,
                    preferred_socket_mapper: None,
                    upload_capabilities: None,
                    download_capabilities: None,
                    socket_mapper_capabilities: None,
                    backend_stats: Default::default(),
                },
                throttles: Default::default(),
            });
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.finish(Effect::Shutdown {
                filters: None,
                traffic: None,
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.sequence, 2);
        worker.join().unwrap();
        drop(first);
    }
    #[test]
    fn resolve_rechecks_both_directions_instead_of_applying_unsupported_limits() {
        let (monitor, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut owner = Owner {
            manager: ThrottleManager::new(None, None),
            config: config::Config::default(),
            monitor,
            save: false,
            pending_resolution: None,
            socket_mapper: None,
            socket_mappers: vec![],
        };
        let request = ThrottleRequest {
            pid: -1,
            name: "test".into(),
            limit: ThrottleLimit {
                upload_limit: Some(100),
                download_limit: Some(100),
                traffic_type: TrafficType::Local,
            },
        };
        assert!(owner.compatibility(&request).unwrap().is_upload);
        let result = owner.apply(
            1,
            Effect::Resolve {
                request: request.clone(),
                is_upload: true,
                action: ui::BackendCompatibilityAction::ConvertToAll,
            },
        );
        assert!(result.dialog.unwrap().is_upload);
        assert!(!result.close_throttle);
        assert!(result.throttles.is_empty());
        let mut download_only = request;
        download_only.limit.upload_limit = None;
        let result = owner.apply(
            2,
            Effect::Resolve {
                request: download_only,
                is_upload: false,
                action: ui::BackendCompatibilityAction::ConvertToAll,
            },
        );
        assert!(!result.dialog.unwrap().is_upload);
        assert!(!result.close_throttle);
        assert_eq!(owner.manager.get_default_backends(), (None, None));
    }
    #[test]
    fn admission_preserves_mutation_then_preferences_then_refresh() {
        let (tx, rx) = std::sync::mpsc::sync_channel(4);
        let mut service = Service {
            tx,
            thread: None,
            sequence: 0,
        };
        let receipts = [
            service
                .request(Effect::Remove {
                    pid: 1,
                    name: "test".into(),
                })
                .unwrap(),
            service
                .request(Effect::Preferences {
                    filters: Some(vec![]),
                    traffic: Some(TrafficType::Local),
                })
                .unwrap(),
            service.request(Effect::Refresh).unwrap(),
        ];
        assert!(matches!(rx.recv().unwrap().effect, Effect::Remove { .. }));
        assert!(matches!(
            rx.recv().unwrap().effect,
            Effect::Preferences { .. }
        ));
        assert!(matches!(rx.recv().unwrap().effect, Effect::Refresh));
        drop(receipts);
    }
}
