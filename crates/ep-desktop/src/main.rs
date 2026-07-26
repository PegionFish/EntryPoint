fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "entrypoint=info,ep_core=info".into()),
        )
        .init();

    tracing::info!("EntryPoint starting...");

    // Config directory
    let config_dir = std::env::current_dir()
        .unwrap_or_default()
        .join("config");
    let _ = std::fs::create_dir_all(&config_dir);

    // Load config on main thread before spawning anything
    let config = ep_core::config::AppConfig::load_or_create(&config_dir)
        .unwrap_or_default();

    // mpsc channel: background → UI
    let (tx, rx) = std::sync::mpsc::channel();

    // unbounded channel: UI → background (tokio unbounded is Send + Clone)
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();

    // Spawn tokio runtime on a dedicated background thread
    let bg_tx = tx.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(background_loop(bg_tx, cmd_rx, config));
    });

    // eframe runs on the main thread
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("EntryPoint")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "EntryPoint",
        native_options,
        Box::new(move |_cc| Ok(Box::new(ep_desktop::App::new(rx, cmd_tx)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

/// Background event loop — owns ProcessManager, PortManager, runs on tokio runtime.
async fn background_loop(
    tx: std::sync::mpsc::Sender<ep_desktop::app::AppMsg>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<ep_desktop::app::AppCmd>,
    config: ep_core::config::AppConfig,
) {
    use ep_desktop::app::{AppCmd, AppMsg};

    let (port_range_start, port_range_end) = config.port_range();
    let mut port_manager = ep_core::port::PortManager::new(port_range_start, port_range_end);
    let mut process_manager = ep_core::process::ProcessManager::new();

    // Initial device detection
    let disabled = &config.compute.disabled_backends;
    let devices = ep_core::compute::detect_all_devices(disabled);
    let _ = tx.send(AppMsg::DevicesRefreshed(devices.clone()));

    // Initial module discovery
    let modules_dir = std::env::current_dir()
        .unwrap_or_default()
        .join("modules");
    let discovered = ep_core::module::discover_modules(&modules_dir);
    let _ = tx.send(AppMsg::ModulesDiscovered(discovered.clone()));

    // Periodic timers
    let mut device_timer = tokio::time::interval(std::time::Duration::from_secs(
        config.compute.refresh_interval_secs.max(2) as u64,
    ));
    device_timer.tick().await; // consume immediate first tick
    let mut monitor_timer = tokio::time::interval(std::time::Duration::from_secs(1));
    monitor_timer.tick().await;

    let mut current_devices = devices;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(AppCmd::StartModule(module_id)) => {
                        let manifest = discovered
                            .iter()
                            .find(|m| {
                                m.manifest
                                    .as_ref()
                                    .map(|mf| mf.module.id == module_id)
                                    .unwrap_or(false)
                            })
                            .and_then(|m| m.manifest.clone());

                        if let Some(manifest) = manifest {
                            match port_manager.allocate(&module_id) {
                                Ok(port) => {
                                    let device = current_devices
                                        .iter()
                                        .find(|d| d.backend != ep_core::types::ComputeBackend::Cpu)
                                        .map(|d| d.id.clone())
                                        .unwrap_or(ep_core::types::DeviceId::Cpu);

                                    match process_manager
                                        .start_module(
                                            &module_id,
                                            &manifest,
                                            device.clone(),
                                            port,
                                            std::collections::HashMap::new(),
                                        )
                                        .await
                                    {
                                        Ok(()) => {
                                            let _ = tx.send(AppMsg::ModuleStarted(
                                                module_id,
                                                port,
                                                device.to_string(),
                                            ));
                                        }
                                        Err(e) => {
                                            port_manager.release(&module_id);
                                            let _ = tx.send(AppMsg::Error(format!(
                                                "启动 {module_id} 失败: {e}"
                                            )));
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(AppMsg::Error(format!(
                                        "端口分配失败: {e}"
                                    )));
                                }
                            }
                        } else {
                            let _ = tx.send(AppMsg::Error(format!(
                                "模块 {module_id} 未找到或 manifest 无效"
                            )));
                        }
                    }
                    Some(AppCmd::StopModule(module_id)) => {
                        let _ = process_manager.stop_module(&module_id).await;
                        port_manager.release(&module_id);
                        let _ = tx.send(AppMsg::ModuleStopped(module_id));
                    }
                    Some(AppCmd::Shutdown) => break,
                    None => break,
                }
            }
            _ = device_timer.tick() => {
                ep_core::compute::refresh_all_devices(
                    &mut current_devices,
                    &config.compute.disabled_backends,
                );
                let _ = tx.send(AppMsg::DevicesRefreshed(current_devices.clone()));
            }
            _ = monitor_timer.tick() => {
                // Check for exited processes and send status updates
                let module_ids: Vec<String> = discovered
                    .iter()
                    .filter_map(|m| {
                        m.manifest.as_ref().map(|mf| mf.module.id.clone())
                    })
                    .collect();
                for mid in &module_ids {
                    let _ = process_manager.monitor_process(mid);
                    if let Some(status) = process_manager.get_status(mid) {
                        let _ = tx.send(AppMsg::ModuleStatusUpdate(
                            mid.clone(),
                            status.clone(),
                        ));
                    }
                }
            }
        }
    }
}
