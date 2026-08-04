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
    use ep_desktop::i18n::tr;
    use ep_core::model::DownloadState;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    // UI 文案语言：从启动时配置归一化（&'static str，可安全移入各异步任务）。
    // 注：设置页的语言切换即时作用于 UI 渲染；后台错误文案在下次启动后跟随新语言。
    let lang = ep_core::i18n::normalize_language(&config.general.language);

    let (port_range_start, port_range_end) = config.port_range();
    let mut port_manager = ep_core::port::PortManager::new(port_range_start, port_range_end);
    // A2（§3.1）：模块子进程注入共享 CUDA 库目录（Linux LD_LIBRARY_PATH 前置 /
    // Windows PATH 前置，平台分支在 process.rs 内部）
    let mut process_manager = ep_core::process::ProcessManager::new().with_cuda_libs_dir(
        ep_core::process::resolve_cuda_libs_dir(&root, &config.compute.cuda_libs_dir),
    );

    // Initial device detection
    let disabled = &config.compute.disabled_backends;
    let devices = ep_core::compute::detect_all_devices(disabled);
    let _ = tx.send(AppMsg::DevicesRefreshed(devices.clone()));

    // Initial module discovery（先于 ModelManager，便于注册 manifests）
    let modules_dir = root.join("modules");
    let mut discovered = ep_core::module::discover_modules(&modules_dir);
    let _ = tx.send(AppMsg::ModulesDiscovered(discovered.clone()));

    // ModelManager：注册模块 manifests（import 解析 target_dir 依赖）+ 网络代理（更新检查依赖）
    let mut model_manager = ep_core::model::ModelManager::new(&config.models, &root)
        .with_network(config.network.clone())
        .with_manifests(manifests_from(&discovered));

    // 进行中的下载句柄（model_id → handle），供取消与进度转发任务清理
    let download_handles: Arc<Mutex<HashMap<String, ep_core::model::DownloadHandle>>> =
        Arc::new(Mutex::new(HashMap::new()));

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

                                    // A2（P0-4 前置）：公共构建函数产出标准模板变量
                                    // （ROOT/MODULE_DIR/...），start_module 统一加 EP_ 前缀
                                    // 并注入 CUDA 库路径 + compute.env，不再传空 map
                                    let env_vars = ep_core::process::build_module_env(
                                        &root, &module_id, &manifest, &device,
                                    );

                                    match process_manager
                                        .start_module(
                                            &module_id,
                                            &manifest,
                                            device.clone(),
                                            port,
                                            env_vars,
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
                                            let _ = tx.send(AppMsg::Error(tr(
                                                lang,
                                                "desktopApp.error.startModuleFailed",
                                                &[
                                                    ("id", &module_id),
                                                    ("detail", &e.to_string()),
                                                ],
                                            )));
                                        }
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(AppMsg::Error(tr(
                                        lang,
                                        "desktopApp.error.portAllocFailed",
                                        &[("detail", &e.to_string())],
                                    )));
                                }
                            }
                        } else {
                            let _ = tx.send(AppMsg::Error(tr(
                                lang,
                                "desktopApp.error.moduleNotFoundOrInvalid",
                                &[("id", &module_id)],
                            )));
                        }
                    }
                    Some(AppCmd::StopModule(module_id)) => {
                        let _ = process_manager.stop_module(&module_id).await;
                        port_manager.release(&module_id);
                        let _ = tx.send(AppMsg::ModuleStopped(module_id));
                    }
                    Some(AppCmd::DownloadModel { module_id, model_id, source }) => {
                        // 在已发现模块中查找 manifest 与对应模型声明
                        let decl = discovered
                            .iter()
                            .filter_map(|m| m.manifest.as_ref())
                            .find(|mf| mf.module.id == module_id)
                            .and_then(|mf| {
                                mf.models.iter().find(|d| d.id == model_id).cloned()
                            });

                        if let Some(decl) = decl {
                            // venv python 解释器路径（A2：process.rs 公共平台分支助手，
                            // 与 start_module/deps 的 venv 路径口径一致）
                            // 注：下载子进程自身的环境变量（网络代理）由 model.rs 统一注入；
                            // 下载脚本不 import torch，无需 CUDA 库路径。
                            let venv_python =
                                ep_core::process::venv_python_path(&root, &module_id);

                            if !venv_python.exists() {
                                let _ = tx.send(AppMsg::Error(tr(
                                    lang,
                                    "desktopApp.error.startModuleFirst",
                                    &[],
                                )));
                                let _ = tx.send(AppMsg::ModelDownloadFinished(
                                    model_id.clone(),
                                    false,
                                ));
                            } else {
                                // 任务化下载：立即返回句柄，进度经转发任务回传，绝不阻塞事件循环
                                match model_manager.execute_download_with_progress(
                                    &module_id,
                                    &decl,
                                    &venv_python,
                                    &config,
                                    source,
                                ) {
                                    Ok(handle) => {
                                        let mut progress_rx = handle.subscribe_progress();
                                        // 保存句柄供取消（UI 发 CancelDownload）
                                        download_handles
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .insert(model_id.clone(), handle);

                                        let tx2 = tx.clone();
                                        let mid = model_id.clone();
                                        let handles2 = Arc::clone(&download_handles);
                                        tokio::spawn(async move {
                                            use tokio::sync::broadcast::error::RecvError;
                                            let mut success = false;
                                            loop {
                                                match progress_rx.recv().await {
                                                    Ok(p) => {
                                                        let terminal = !matches!(
                                                            p.state,
                                                            DownloadState::Downloading
                                                        );
                                                        let _ = tx2.send(
                                                            AppMsg::ModelDownloadProgress {
                                                                model_id: mid.clone(),
                                                                percent: p.percent,
                                                                bytes: p.bytes,
                                                                state: p.state.clone(),
                                                            },
                                                        );
                                                        if terminal {
                                                            match &p.state {
                                                                DownloadState::Completed => {
                                                                    success = true;
                                                                }
                                                                DownloadState::Failed(msg) => {
                                                                    let _ = tx2.send(
                                                                        AppMsg::Error(tr(
                                                                            lang,
                                                                            "desktopApp.error.downloadFailed",
                                                                            &[
                                                                                ("id", &mid),
                                                                                ("detail", msg),
                                                                            ],
                                                                        )),
                                                                    );
                                                                }
                                                                DownloadState::Cancelled => {
                                                                    let _ = tx2.send(
                                                                        AppMsg::Info(tr(
                                                                            lang,
                                                                            "desktopApp.error.downloadCancelled",
                                                                            &[("id", &mid)],
                                                                        )),
                                                                    );
                                                                }
                                                                DownloadState::Downloading => {}
                                                            }
                                                            break;
                                                        }
                                                    }
                                                    // 接收滞后：跳过丢失的事件，继续等待后续进度
                                                    Err(RecvError::Lagged(_)) => continue,
                                                    // 通道已关闭：按异常结束处理
                                                    Err(RecvError::Closed) => break,
                                                }
                                            }
                                            // 清理句柄并发出最终消息（UI 据此刷新列表）
                                            handles2
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .remove(&mid);
                                            let _ = tx2.send(AppMsg::ModelDownloadFinished(
                                                mid, success,
                                            ));
                                        });
                                    }
                                    Err(e) => {
                                        let _ = tx.send(AppMsg::Error(tr(
                                            lang,
                                            "desktopApp.error.startDownloadFailed",
                                            &[("detail", &e.to_string())],
                                        )));
                                        let _ = tx.send(AppMsg::ModelDownloadFinished(
                                            model_id.clone(),
                                            false,
                                        ));
                                    }
                                }
                            }
                        } else {
                            let _ = tx.send(AppMsg::Error(tr(
                                lang,
                                "desktopApp.error.moduleOrModelNotFound",
                                &[("module", &module_id), ("model", &model_id)],
                            )));
                            let _ = tx
                                .send(AppMsg::ModelDownloadFinished(model_id.clone(), false));
                        }
                    }
                    Some(AppCmd::CancelDownload(model_id)) => {
                        // 从句柄映射中取出引用并取消（cancel 幂等；supervise 任务会发 Cancelled）
                        let mut guard = download_handles.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(handle) = guard.get_mut(&model_id) {
                            handle.cancel();
                        }
                    }
                    Some(AppCmd::CheckUpdate { module_id, model_id }) => {
                        let decl = discovered
                            .iter()
                            .filter_map(|m| m.manifest.as_ref())
                            .find(|mf| mf.module.id == module_id)
                            .and_then(|mf| {
                                mf.models.iter().find(|d| d.id == model_id).cloned()
                            });
                        if let Some(decl) = decl {
                            // spawn 独立任务，避免阻塞命令循环；用独立 ModelManager（同一配置）
                            let models_cfg = config.models.clone();
                            let network = config.network.clone();
                            let root2 = root.clone();
                            let tx2 = tx.clone();
                            tokio::spawn(async move {
                                let mgr = ep_core::model::ModelManager::new(&models_cfg, &root2)
                                    .with_network(network);
                                let result = mgr.check_update_available(&decl).await;
                                let _ = tx2.send(AppMsg::ModelUpdateChecked {
                                    model_id,
                                    result,
                                    notify: true,
                                });
                            });
                        } else {
                            let _ = tx.send(AppMsg::Error(tr(
                                lang,
                                "desktopApp.error.moduleOrModelNotFoundUpdate",
                                &[("module", &module_id), ("model", &model_id)],
                            )));
                        }
                    }
                    Some(AppCmd::CheckAllUpdates) => {
                        // 收集所有 Ready 模型的声明，spawn 单任务内并发检查并汇总
                        let ready_models = model_manager
                            .list_all_models(&manifests_from(&discovered))
                            .into_iter()
                            .filter(|mv| mv.status == ep_core::model::ModelStatus::Ready)
                            .collect::<Vec<_>>();

                        let decls: Vec<(String, ep_core::module::ModelDecl)> = ready_models
                            .iter()
                            .filter_map(|mv| {
                                discovered
                                    .iter()
                                    .filter_map(|m| m.manifest.as_ref())
                                    .find(|mf| mf.module.id == mv.module_id)
                                    .and_then(|mf| {
                                        mf.models
                                            .iter()
                                            .find(|d| d.id == mv.model_id)
                                            .map(|d| (mv.model_id.clone(), d.clone()))
                                    })
                            })
                            .collect();

                        // 各模型各自 spawn 并发检查（JoinSet），完成后汇总
                        let models_cfg = config.models.clone();
                        let network = config.network.clone();
                        let root2 = root.clone();
                        let tx2 = tx.clone();
                        tokio::spawn(async move {
                            let mut set = tokio::task::JoinSet::new();
                            for (model_id, decl) in decls {
                                let models_cfg = models_cfg.clone();
                                let network = network.clone();
                                let root2 = root2.clone();
                                set.spawn(async move {
                                    let mgr =
                                        ep_core::model::ModelManager::new(&models_cfg, &root2)
                                            .with_network(network);
                                    let result = mgr.check_update_available(&decl).await;
                                    (model_id, result)
                                });
                            }
                            let total = set.len();
                            let mut available = 0usize;
                            while let Some(joined) = set.join_next().await {
                                if let Ok((model_id, result)) = joined {
                                    if result.available {
                                        available += 1;
                                    }
                                    let _ = tx2.send(AppMsg::ModelUpdateChecked {
                                        model_id,
                                        result,
                                        notify: false,
                                    });
                                }
                            }
                            let _ = tx2.send(AppMsg::UpdatesCheckSummary { total, available });
                        });
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
                                let _ = tx.send(AppMsg::Error(tr(
                                    lang,
                                    "desktopApp.error.deleteModelFailed",
                                    &[("detail", &e.to_string())],
                                )));
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
                                let _ = tx.send(AppMsg::Error(tr(
                                    lang,
                                    "desktopApp.error.importModelFailed",
                                    &[("detail", &e.to_string())],
                                )));
                            }
                        }
                    }
                    Some(AppCmd::RefreshModels) => {
                        // 重新扫描模块目录并刷新模型列表；同步更新 ModelManager 注册的 manifests
                        discovered = ep_core::module::discover_modules(&modules_dir);
                        model_manager.set_manifests(manifests_from(&discovered));
                        let _ = tx.send(AppMsg::ModelsRefreshed(
                            model_manager.list_all_models(&manifests_from(&discovered)),
                        ));
                    }
                    Some(AppCmd::RefreshDeps) => {
                        let report = ep_core::deps::DepReport::check_all(&root);
                        let _ = tx.send(AppMsg::DepReportRefreshed(report));
                    }
                    // ── Wave S S2 骨架占位分支（注册点预登记；Wave 3 C4 填实现） ──
                    Some(AppCmd::RefreshPacks) => {
                        // TODO(C4): ep-pack 注册表查询（runtime/packs/*.json）→ AppMsg::PacksRefreshed
                        tracing::debug!("RefreshPacks: skeleton stub, C4 implements");
                    }
                    Some(AppCmd::ImportPack { .. }) => {
                        // TODO(C4): ep-pack 导入编排（§4.4：暂存/校验/落位/注册），
                        // 进度经 AppMsg::PackImportProgress / PackImportFinished 上报
                        tracing::debug!("ImportPack: skeleton stub, C4 implements");
                    }
                    Some(AppCmd::ExecuteSingle { .. }) => {
                        // TODO(C4): ep-core 直连 submit_direct（§5.3 退化三节点 DAG），
                        // 成功后发 AppMsg::DirectExecSubmitted(task_id)
                        tracing::debug!("ExecuteSingle: skeleton stub, C4 implements");
                    }
                    Some(AppCmd::RefreshPipelineTasks { .. }) => {
                        // TODO(C4): ep-core 任务注册表按 pipeline_id 查询（§6.8）
                        // → AppMsg::PipelineTasksRefreshed
                        tracing::debug!("RefreshPipelineTasks: skeleton stub, C4 implements");
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
