fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "entrypoint=info,ep_core=info".into()),
        )
        .init();

    tracing::info!("EntryPoint starting...");

    // Resolve project root and config directory
    let root = ep_core::config::resolve_root();
    let config_dir = root.join("config");
    let _ = std::fs::create_dir_all(&config_dir);

    // Load config on main thread before spawning anything
    let config = ep_core::config::AppConfig::load_or_create(&config_dir)
        .unwrap_or_default();

    // 克隆一份配置供 UI 使用（原配置随后移入后台线程）
    let ui_config = config.clone();

    // mpsc channel: background → UI
    let (tx, rx) = std::sync::mpsc::channel();

    // unbounded channel: UI → background (tokio unbounded is Send + Clone)
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();

    // Spawn tokio runtime on a dedicated background thread
    let bg_tx = tx.clone();
    let bg_root = root.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(background_loop(bg_tx, cmd_rx, config, bg_root));
    });

    // eframe runs on the main thread
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("EntryPoint")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "EntryPoint",
        native_options,
        Box::new(move |cc| {
            configure_fonts(&cc.egui_ctx);
            // 应用配置中的字体大小与整体缩放（egui 0.31 的 API 为 set_zoom_factor）
            ep_desktop::theme::apply_font_size(&cc.egui_ctx, ui_config.ui.font_size);
            cc.egui_ctx.set_zoom_factor(ui_config.ui.scale_factor);
            Ok(Box::new(ep_desktop::App::new(rx, cmd_tx, ui_config)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}

/// Load CJK fonts so Chinese text renders correctly.
fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Try system CJK fonts in order of preference (Windows + Linux)
    let cjk_font_paths = [
        // Windows
        "C:\\Windows\\Fonts\\msyh.ttc",   // Microsoft YaHei
        "C:\\Windows\\Fonts\\msyhbd.ttc",  // Microsoft YaHei Bold
        "C:\\Windows\\Fonts\\simsun.ttc",  // SimSun
        "C:\\Windows\\Fonts\\simhei.ttf",  // SimHei
        // Linux
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/wqy-microhei/wqy-microhei.ttc",
    ];

    for path in &cjk_font_paths {
        if let Ok(font_data) = std::fs::read(path) {
            fonts.font_data.insert(
                "cjk".to_owned(),
                egui::FontData::from_owned(font_data).into(),
            );
            // Prepend CJK font to all font families so it's used for CJK glyphs
            for family in [
                egui::FontFamily::Proportional,
                egui::FontFamily::Monospace,
            ] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, "cjk".to_owned());
            }
            tracing::info!("Loaded CJK font from {path}");
            break;
        }
    }

    ctx.set_fonts(fonts);
}

/// 从模块发现结果中提取有效的 manifest 列表
fn manifests_from(
    discovered: &[ep_core::module::DiscoveredModule],
) -> Vec<ep_core::module::ModuleManifest> {
    discovered.iter().filter_map(|m| m.manifest.clone()).collect()
}

/// Background event loop — owns ProcessManager, PortManager, runs on tokio runtime.
async fn background_loop(
    tx: std::sync::mpsc::Sender<ep_desktop::app::AppMsg>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<ep_desktop::app::AppCmd>,
    config: ep_core::config::AppConfig,
    root: std::path::PathBuf,
) {
    use ep_desktop::app::{AppCmd, AppMsg};

    let (port_range_start, port_range_end) = config.port_range();
    let mut port_manager = ep_core::port::PortManager::new(port_range_start, port_range_end);
    let mut process_manager = ep_core::process::ProcessManager::new();
    let mut model_manager = ep_core::model::ModelManager::new(&config.models, &root);

    // Initial device detection
    let disabled = &config.compute.disabled_backends;
    let devices = ep_core::compute::detect_all_devices(disabled);
    let _ = tx.send(AppMsg::DevicesRefreshed(devices.clone()));

    // Initial module discovery
    let modules_dir = root.join("modules");
    let mut discovered = ep_core::module::discover_modules(&modules_dir);
    let _ = tx.send(AppMsg::ModulesDiscovered(discovered.clone()));

    // 启动时自动检查依赖并刷新模型列表
    let _ = tx.send(AppMsg::DepReportRefreshed(
        ep_core::deps::DepReport::check_all(&root),
    ));
    let _ = tx.send(AppMsg::ModelsRefreshed(
        model_manager.list_all_models(&manifests_from(&discovered)),
    ));

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
                    Some(AppCmd::DownloadModel(module_id, model_id)) => {
                        // 在已发现模块中查找 manifest 与对应模型声明
                        let decl = discovered
                            .iter()
                            .filter_map(|m| m.manifest.as_ref())
                            .find(|mf| mf.module.id == module_id)
                            .and_then(|mf| {
                                mf.models.iter().find(|d| d.id == model_id).cloned()
                            });

                        if let Some(decl) = decl {
                            // venv python 解释器路径（Windows: Scripts/python.exe，其他: bin/python）
                            let venv_python = if cfg!(target_os = "windows") {
                                root.join("runtime")
                                    .join("venvs")
                                    .join(&module_id)
                                    .join("Scripts")
                                    .join("python.exe")
                            } else {
                                root.join("runtime")
                                    .join("venvs")
                                    .join(&module_id)
                                    .join("bin")
                                    .join("python")
                            };

                            if !venv_python.exists() {
                                let _ = tx.send(AppMsg::ModelDownloadFinished(
                                    model_id.clone(),
                                    false,
                                ));
                                let _ = tx.send(AppMsg::Error(
                                    "请先启动一次该模块以准备 Python 环境，然后再下载模型"
                                        .to_string(),
                                ));
                            } else {
                                // 长耗时操作，直接在当前分支 await
                                let module_dir = root.join("modules").join(&module_id);
                                match model_manager
                                    .execute_download(&decl, &module_dir, &venv_python, &config)
                                    .await
                                {
                                    Ok(()) => {
                                        let _ = tx.send(AppMsg::ModelDownloadFinished(
                                            model_id.clone(),
                                            true,
                                        ));
                                        let _ = tx.send(AppMsg::ModelsRefreshed(
                                            model_manager
                                                .list_all_models(&manifests_from(&discovered)),
                                        ));
                                    }
                                    Err(e) => {
                                        let _ = tx.send(AppMsg::ModelDownloadFinished(
                                            model_id.clone(),
                                            false,
                                        ));
                                        let _ = tx.send(AppMsg::Error(format!(
                                            "模型下载失败: {e}"
                                        )));
                                    }
                                }
                            }
                        } else {
                            let _ = tx
                                .send(AppMsg::ModelDownloadFinished(model_id.clone(), false));
                            let _ = tx.send(AppMsg::Error(format!(
                                "模块 {module_id} 或模型 {model_id} 未找到"
                            )));
                        }
                    }
                    Some(AppCmd::DeleteModel(target_dir)) => {
                        let dir = model_manager.model_dir(&target_dir);
                        match tokio::fs::remove_dir_all(&dir).await {
                            Ok(()) => {
                                let _ = tx.send(AppMsg::ModelsRefreshed(
                                    model_manager.list_all_models(&manifests_from(&discovered)),
                                ));
                            }
                            Err(e) => {
                                let _ = tx.send(AppMsg::Error(format!("删除模型失败: {e}")));
                            }
                        }
                    }
                    Some(AppCmd::ImportModel {
                        module_id,
                        model_id,
                        source,
                    }) => {
                        match model_manager
                            .import_model(&module_id, &model_id, &source)
                            .await
                        {
                            Ok(()) => {
                                let _ = tx.send(AppMsg::ModelsRefreshed(
                                    model_manager.list_all_models(&manifests_from(&discovered)),
                                ));
                            }
                            Err(e) => {
                                let _ = tx.send(AppMsg::Error(format!("导入模型失败: {e}")));
                            }
                        }
                    }
                    Some(AppCmd::RefreshModels) => {
                        // 重新扫描模块目录并刷新模型列表
                        discovered = ep_core::module::discover_modules(&modules_dir);
                        let _ = tx.send(AppMsg::ModelsRefreshed(
                            model_manager.list_all_models(&manifests_from(&discovered)),
                        ));
                    }
                    Some(AppCmd::RefreshDeps) => {
                        let report = ep_core::deps::DepReport::check_all(&root);
                        let _ = tx.send(AppMsg::DepReportRefreshed(report));
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
                    let _ = process_manager.monitor_process(mid).await;
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
