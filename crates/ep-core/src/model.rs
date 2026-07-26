//! 模型下载管理 — ModelManager
//!
//! 负责模型缓存目录管理、元数据读写、下载命令构建。
//! 不实际执行下载（只构建命令），实际执行由 ProcessManager / UI 层驱动。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Child;
use tracing::{debug, info, warn};

use crate::config::ModelsConfig;
use crate::module::manifest::{ModelDecl, ModelSource};

// ─── 常量 ────────────────────────────────────────────────────────────────────

const META_FILE_NAME: &str = ".ep_meta.json";

// ─── ModelMeta ───────────────────────────────────────────────────────────────

/// 模型元数据，对应 `.ep_meta.json` 文件。
///
/// 位于模型缓存目录内每个模型文件夹下。
/// 用户可安全删除此文件——删除后系统视为手动放置的模型，直接使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMeta {
    /// 所属模块 ID
    pub module_id: String,
    /// 模型 ID（对应 module.toml 中 [[models]].id）
    pub model_id: String,
    /// 下载源（huggingface / modelscope / url）
    pub source: String,
    /// 仓库 ID
    pub repo_id: String,
    /// 版本/分支
    pub revision: String,
    /// 下载完成时间（ISO 8601）
    pub downloaded_at: String,
    /// 总大小（字节）
    pub total_size_bytes: u64,
}

// ─── DownloadedModel ─────────────────────────────────────────────────────────

/// 已下载模型的摘要信息（用于列表展示）
#[derive(Debug, Clone)]
pub struct DownloadedModel {
    /// 相对于 cache_dir 的目录名
    pub target_dir: String,
    /// 元数据
    pub meta: ModelMeta,
    /// 目录总大小（字节）。简化实现：当前固定为 0，后续可递归统计。
    pub size_bytes: u64,
}

// ─── ModelManager ────────────────────────────────────────────────────────────

/// 模型下载管理器
///
/// 管理模型缓存目录、元数据文件、下载命令构建。
pub struct ModelManager {
    /// 模型缓存根目录（已解析为绝对路径）
    cache_dir: PathBuf,
    /// HuggingFace 镜像站 URL（空字符串 = 官方源）
    hf_endpoint: String,
    /// 默认下载源（huggingface / modelscope）
    default_source: String,
    /// 当前正在执行的下载子进程（用于取消）
    download_child: Option<Child>,
}

impl ModelManager {
    /// 从配置创建 ModelManager。
    ///
    /// `cache_dir` 为相对路径时基于 `root` 解析。
    pub fn new(config: &ModelsConfig, root: &Path) -> Self {
        let cache_path = Path::new(&config.cache_dir);
        let cache_dir = if cache_path.is_absolute() {
            cache_path.to_path_buf()
        } else {
            root.join(cache_path)
        };

        debug!(
            cache_dir = %cache_dir.display(),
            hf_endpoint = %config.hf_endpoint,
            default_source = %config.default_source,
            "ModelManager initialized"
        );

        Self {
            cache_dir,
            hf_endpoint: config.hf_endpoint.clone(),
            default_source: config.default_source.clone(),
            download_child: None,
        }
    }

    /// 返回模型的完整缓存路径：`cache_dir / target_dir`
    pub fn model_dir(&self, target_dir: &str) -> PathBuf {
        self.cache_dir.join(target_dir)
    }

    /// 检查模型是否已存在（目录存在且非空）
    pub fn is_model_present(&self, target_dir: &str) -> bool {
        let dir = self.model_dir(target_dir);
        if !dir.is_dir() {
            return false;
        }
        // 目录存在且至少包含一个条目
        match fs::read_dir(&dir) {
            Ok(mut entries) => entries.next().is_some(),
            Err(_) => false,
        }
    }

    /// 读取模型的 `.ep_meta.json` 元数据
    pub fn read_meta(&self, target_dir: &str) -> Option<ModelMeta> {
        let meta_path = self.model_dir(target_dir).join(META_FILE_NAME);
        if !meta_path.is_file() {
            return None;
        }
        let content = match fs::read_to_string(&meta_path) {
            Ok(c) => c,
            Err(e) => {
                warn!(path = %meta_path.display(), error = %e, "failed to read meta file");
                return None;
            }
        };
        match serde_json::from_str(&content) {
            Ok(meta) => Some(meta),
            Err(e) => {
                warn!(path = %meta_path.display(), error = %e, "failed to parse meta file");
                None
            }
        }
    }

    /// 写入模型的 `.ep_meta.json` 元数据
    pub fn write_meta(&self, target_dir: &str, meta: &ModelMeta) -> Result<()> {
        let dir = self.model_dir(target_dir);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create model dir {}", dir.display()))?;

        let meta_path = dir.join(META_FILE_NAME);
        let content =
            serde_json::to_string_pretty(meta).context("failed to serialize model meta")?;
        fs::write(&meta_path, content)
            .with_context(|| format!("failed to write {}", meta_path.display()))?;

        debug!(path = %meta_path.display(), "model meta written");
        Ok(())
    }

    /// 构建模型下载命令（不实际执行）。
    ///
    /// 返回 `(program, args)`：
    /// - `program`：venv 中的 python 解释器路径
    /// - `args`：`["-c", "<python code>"]`
    ///
    /// - HuggingFace：使用 `huggingface_hub.snapshot_download`
    /// - ModelScope：使用 `modelscope.snapshot_download`
    /// - 如果配置了 `hf_endpoint`，在 Python 代码中设置 `HF_ENDPOINT` 环境变量
    ///
    /// TODO: 实际执行下载、进度解析、断点续传由上层（ProcessManager / UI）驱动
    pub fn build_download_command(
        &self,
        model: &ModelDecl,
        venv_python: &Path,
    ) -> (String, Vec<String>) {
        let local_dir = self.model_dir(&model.target_dir);
        // 将路径转为正斜杠字符串，避免 Windows 反斜杠在 Python 字符串中的转义问题
        let local_dir_str = local_dir.to_string_lossy().replace('\\', "/");

        let python_code = match model.source {
            ModelSource::Huggingface => {
                let repo_id = model.repo_id.as_deref().unwrap_or_default();
                let revision = model.revision.as_deref().unwrap_or("main");

                let mut parts: Vec<String> = Vec::new();

                // 如果配置了 HF 镜像，在导入前设置环境变量
                if !self.hf_endpoint.is_empty() {
                    parts.push(format!(
                        "import os; os.environ['HF_ENDPOINT']='{}'",
                        self.hf_endpoint
                    ));
                }

                parts.push("from huggingface_hub import snapshot_download".to_string());
                parts.push(format!(
                    "snapshot_download(repo_id='{}', local_dir='{}', revision='{}')",
                    repo_id, local_dir_str, revision
                ));

                parts.join("; ")
            }
            ModelSource::Modelscope => {
                let repo_id = model.repo_id.as_deref().unwrap_or_default();
                let revision = model.revision.as_deref().unwrap_or("master");

                format!(
                    "from modelscope import snapshot_download; snapshot_download('{}', local_dir='{}', revision='{}')",
                    repo_id, local_dir_str, revision
                )
            }
            ModelSource::Url => {
                // URL 直链下载由上层使用 reqwest 实现，此处返回占位命令
                // TODO: 实现 URL 直链下载（reqwest 流式下载 + 进度回调）
                let url = model.url.as_deref().unwrap_or_default();
                format!(
                    "import sys; print('URL download not yet implemented: {}', file=sys.stderr); sys.exit(1)",
                    url
                )
            }
        };

        let program = venv_python.to_string_lossy().to_string();
        let args = vec!["-c".to_string(), python_code];

        debug!(
            program = %program,
            source = ?model.source,
            repo_id = ?model.repo_id,
            "download command built"
        );

        (program, args)
    }

    /// 检查模型是否有可用更新。
    ///
    /// 读取本地 `.ep_meta.json` 的 revision，与远端对比。
    ///
    /// TODO: 实际需要网络请求对比远端最新 revision
    ///   - HuggingFace: GET /api/models/{repo_id}/revision/main
    ///   - ModelScope: 类似 API
    ///   当前暂时返回 Ok(false)
    pub fn check_update_available(&self, model: &ModelDecl) -> Result<bool> {
        let _meta = self.read_meta(&model.target_dir);

        // TODO: 实现远端 revision 对比
        // 1. 从 meta 中取本地 revision
        // 2. 请求远端 API 获取最新 revision
        // 3. 对比是否一致
        // 当前阶段无网络请求，直接返回 false
        Ok(false)
    }

    /// 扫描 cache_dir 下所有含 `.ep_meta.json` 的目录，返回已下载模型列表。
    pub fn list_downloaded_models(&self) -> Vec<DownloadedModel> {
        let mut models = Vec::new();

        let entries = match fs::read_dir(&self.cache_dir) {
            Ok(entries) => entries,
            Err(e) => {
                debug!(
                    cache_dir = %self.cache_dir.display(),
                    error = %e,
                    "cannot read model cache dir (may not exist yet)"
                );
                return models;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let meta_path = path.join(META_FILE_NAME);
            if !meta_path.is_file() {
                continue;
            }

            let target_dir = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            match self.read_meta(&target_dir) {
                Some(meta) => {
                    models.push(DownloadedModel {
                        target_dir,
                        meta,
                        // TODO: 递归统计目录大小，当前简化为 0
                        size_bytes: 0,
                    });
                }
                None => {
                    warn!(dir = %target_dir, "meta file exists but failed to parse, skipping");
                }
            }
        }

        models
    }

    /// 获取缓存根目录
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// 获取默认下载源
    pub fn default_source(&self) -> &str {
        &self.default_source
    }

    /// 实际执行模型下载
    ///
    /// 调用 `build_download_command()` 获取命令，用 `tokio::process::Command` 执行。
    /// 下载进程句柄保存在 `download_child` 中，可通过 `cancel_download()` 取消。
    pub async fn execute_download(
        &mut self,
        model: &ModelDecl,
        _module_dir: &Path,
        venv_python: &Path,
        _config: &crate::config::AppConfig,
    ) -> Result<()> {
        let (program, args) = self.build_download_command(model, venv_python);

        info!(
            model_id = %model.id,
            program = %program,
            "executing model download"
        );

        let child = tokio::process::Command::new(&program)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn download for model '{}'", model.id))?;

        self.download_child = Some(child);

        // Wait for the download to complete
        if let Some(child) = self.download_child.take() {
            let output = child.wait_with_output().await.with_context(|| {
                format!("failed to wait for download of model '{}'", model.id)
            })?;

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            if !stdout.is_empty() {
                debug!(model_id = %model.id, stdout = %stdout.trim(), "download stdout");
            }
            if !stderr.is_empty() {
                debug!(model_id = %model.id, stderr = %stderr.trim(), "download stderr");
            }

            if !output.status.success() {
                anyhow::bail!(
                    "download of model '{}' failed with exit code {:?}: {}",
                    model.id,
                    output.status.code(),
                    stderr.trim()
                );
            }
        } else {
            anyhow::bail!("download process for model '{}' was lost", model.id);
        }

        Ok(())
    }

    /// 取消正在进行的下载（kill 进程）
    pub async fn cancel_download(&mut self) {
        if let Some(mut child) = self.download_child.take() {
            info!("cancelling model download");
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建测试用 ModelsConfig
    fn test_config(cache_dir: &str) -> ModelsConfig {
        ModelsConfig {
            cache_dir: cache_dir.to_string(),
            hf_endpoint: String::new(),
            default_source: "huggingface".to_string(),
            max_concurrent_downloads: 2,
        }
    }

    /// 创建测试用 ModelDecl (HuggingFace)
    fn test_hf_model() -> ModelDecl {
        ModelDecl {
            id: "large-v3".to_string(),
            name: "Whisper Large V3".to_string(),
            source: ModelSource::Huggingface,
            repo_id: Some("Systran/faster-whisper-large-v3".to_string()),
            url: None,
            target_dir: "faster-whisper-large-v3".to_string(),
            revision: Some("main".to_string()),
            size_estimate_mb: Some(3100),
            default: true,
        }
    }

    /// 创建测试用 ModelDecl (ModelScope)
    fn test_ms_model() -> ModelDecl {
        ModelDecl {
            id: "qwen3-asr".to_string(),
            name: "Qwen3 ASR".to_string(),
            source: ModelSource::Modelscope,
            repo_id: Some("Qwen/Qwen3-ASR".to_string()),
            url: None,
            target_dir: "qwen3-asr".to_string(),
            revision: None,
            size_estimate_mb: Some(2000),
            default: false,
        }
    }

    fn test_meta() -> ModelMeta {
        ModelMeta {
            module_id: "faster-whisper".to_string(),
            model_id: "large-v3".to_string(),
            source: "huggingface".to_string(),
            repo_id: "Systran/faster-whisper-large-v3".to_string(),
            revision: "main".to_string(),
            downloaded_at: "2026-07-20T10:30:00Z".to_string(),
            total_size_bytes: 3_094_850_000,
        }
    }

    /// 创建临时目录用于测试，返回路径
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ep_model_test_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    // ── model_dir 路径拼接 ──────────────────────────────────────────────

    #[test]
    fn test_model_dir_relative() {
        let root = Path::new("G:/EntryPoint");
        let config = test_config("models");
        let mgr = ModelManager::new(&config, root);

        assert_eq!(
            mgr.model_dir("faster-whisper-large-v3"),
            PathBuf::from("G:/EntryPoint/models/faster-whisper-large-v3")
        );
    }

    #[test]
    fn test_model_dir_absolute() {
        let root = Path::new("G:/EntryPoint");
        let config = test_config("D:/AI_Models");
        let mgr = ModelManager::new(&config, root);

        // 绝对路径不基于 root
        assert_eq!(
            mgr.model_dir("some-model"),
            PathBuf::from("D:/AI_Models/some-model")
        );
    }

    // ── is_model_present ────────────────────────────────────────────────

    #[test]
    fn test_is_model_present_nonexistent() {
        let dir = temp_dir("present_no");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        assert!(!mgr.is_model_present("no-such-model"));
        cleanup(&dir);
    }

    #[test]
    fn test_is_model_present_empty_dir() {
        let dir = temp_dir("present_empty");
        let model_path = dir.join("empty-model");
        fs::create_dir_all(&model_path).unwrap();

        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        // 空目录 → false
        assert!(!mgr.is_model_present("empty-model"));
        cleanup(&dir);
    }

    #[test]
    fn test_is_model_present_with_files() {
        let dir = temp_dir("present_yes");
        let model_path = dir.join("my-model");
        fs::create_dir_all(&model_path).unwrap();
        fs::write(model_path.join("model.bin"), b"fake").unwrap();

        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        assert!(mgr.is_model_present("my-model"));
        cleanup(&dir);
    }

    // ── build_download_command ──────────────────────────────────────────

    #[test]
    fn test_build_download_command_huggingface() {
        let root = Path::new("G:/EntryPoint");
        let config = test_config("models");
        let mgr = ModelManager::new(&config, root);
        let model = test_hf_model();
        let venv_python = Path::new("G:/EntryPoint/runtime/venvs/faster-whisper/python.exe");

        let (program, args) = mgr.build_download_command(&model, venv_python);

        assert_eq!(program, "G:/EntryPoint/runtime/venvs/faster-whisper/python.exe");
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "-c");

        let code = &args[1];
        assert!(code.contains("from huggingface_hub import snapshot_download"), "code: {code}");
        assert!(code.contains("Systran/faster-whisper-large-v3"), "code: {code}");
        assert!(code.contains("faster-whisper-large-v3"), "code: {code}");
        assert!(code.contains("revision='main'"), "code: {code}");
        // 无镜像时不应设置 HF_ENDPOINT
        assert!(!code.contains("HF_ENDPOINT"), "code: {code}");
    }

    #[test]
    fn test_build_download_command_huggingface_with_mirror() {
        let root = Path::new("G:/EntryPoint");
        let mut config = test_config("models");
        config.hf_endpoint = "https://hf-mirror.com".to_string();
        let mgr = ModelManager::new(&config, root);
        let model = test_hf_model();
        let venv_python = Path::new("python");

        let (_program, args) = mgr.build_download_command(&model, venv_python);
        let code = &args[1];

        assert!(code.contains("HF_ENDPOINT"), "should set HF_ENDPOINT, code: {code}");
        assert!(code.contains("hf-mirror.com"), "code: {code}");
    }

    #[test]
    fn test_build_download_command_modelscope() {
        let root = Path::new("G:/EntryPoint");
        let config = test_config("models");
        let mgr = ModelManager::new(&config, root);
        let model = test_ms_model();
        let venv_python = Path::new("python");

        let (_program, args) = mgr.build_download_command(&model, venv_python);
        let code = &args[1];

        assert!(code.contains("from modelscope import snapshot_download"), "code: {code}");
        assert!(code.contains("Qwen/Qwen3-ASR"), "code: {code}");
        // ModelScope 默认 revision 为 master
        assert!(code.contains("revision='master'"), "code: {code}");
    }

    // ── read_meta / write_meta 往返 ────────────────────────────────────

    #[test]
    fn test_meta_roundtrip() {
        let dir = temp_dir("meta_rt");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        let meta = test_meta();
        mgr.write_meta("faster-whisper-large-v3", &meta).unwrap();

        let loaded = mgr.read_meta("faster-whisper-large-v3").expect("meta should exist");
        assert_eq!(loaded.module_id, meta.module_id);
        assert_eq!(loaded.model_id, meta.model_id);
        assert_eq!(loaded.source, meta.source);
        assert_eq!(loaded.repo_id, meta.repo_id);
        assert_eq!(loaded.revision, meta.revision);
        assert_eq!(loaded.downloaded_at, meta.downloaded_at);
        assert_eq!(loaded.total_size_bytes, meta.total_size_bytes);

        cleanup(&dir);
    }

    #[test]
    fn test_read_meta_nonexistent() {
        let dir = temp_dir("meta_no");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        assert!(mgr.read_meta("ghost-model").is_none());
        cleanup(&dir);
    }

    #[test]
    fn test_read_meta_invalid_json() {
        let dir = temp_dir("meta_bad");
        let model_path = dir.join("bad-model");
        fs::create_dir_all(&model_path).unwrap();
        fs::write(model_path.join(META_FILE_NAME), "not valid json {{{").unwrap();

        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        assert!(mgr.read_meta("bad-model").is_none());
        cleanup(&dir);
    }

    // ── check_update_available ──────────────────────────────────────────

    #[test]
    fn test_check_update_returns_false() {
        let dir = temp_dir("update");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));
        let model = test_hf_model();

        // TODO 阶段始终返回 false
        assert!(!mgr.check_update_available(&model).unwrap());
        cleanup(&dir);
    }

    // ── list_downloaded_models ──────────────────────────────────────────

    #[test]
    fn test_list_downloaded_models_empty() {
        let dir = temp_dir("list_empty");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        assert!(mgr.list_downloaded_models().is_empty());
        cleanup(&dir);
    }

    #[test]
    fn test_list_downloaded_models_nonexistent_dir() {
        let config = test_config("Z:/nonexistent/path/models");
        let mgr = ModelManager::new(&config, Path::new("."));

        // 目录不存在时返回空列表，不 panic
        assert!(mgr.list_downloaded_models().is_empty());
    }

    #[test]
    fn test_list_downloaded_models_with_entries() {
        let dir = temp_dir("list_models");
        let config = test_config(dir.to_str().unwrap());
        let mgr = ModelManager::new(&config, Path::new("."));

        // 写入两个有效模型
        let meta1 = test_meta();
        mgr.write_meta("model-a", &meta1).unwrap();

        let meta2 = ModelMeta {
            module_id: "qwen3-asr".to_string(),
            model_id: "qwen3-asr".to_string(),
            source: "modelscope".to_string(),
            repo_id: "Qwen/Qwen3-ASR".to_string(),
            revision: "master".to_string(),
            downloaded_at: "2026-07-21T08:00:00Z".to_string(),
            total_size_bytes: 2_000_000_000,
        };
        mgr.write_meta("model-b", &meta2).unwrap();

        // 创建一个无 meta 的目录（手动放置的模型，不应出现在列表中）
        fs::create_dir_all(dir.join("manual-model")).unwrap();
        fs::write(dir.join("manual-model/model.bin"), b"data").unwrap();

        let list = mgr.list_downloaded_models();
        assert_eq!(list.len(), 2);

        let dirs: Vec<&str> = list.iter().map(|m| m.target_dir.as_str()).collect();
        assert!(dirs.contains(&"model-a"));
        assert!(dirs.contains(&"model-b"));
        assert!(!dirs.contains(&"manual-model"));

        // size_bytes 简化为 0
        for m in &list {
            assert_eq!(m.size_bytes, 0);
        }

        cleanup(&dir);
    }

    // ── execute_download / cancel_download tests ─────────────────────────

    #[tokio::test]
    async fn test_execute_download_fails_with_invalid_python() {
        let dir = temp_dir("exec_dl");
        let config = test_config(dir.to_str().unwrap());
        let mut mgr = ModelManager::new(&config, Path::new("."));
        let model = test_hf_model();
        let app_config = crate::config::AppConfig::default();

        // 使用不存在的 python 路径 → spawn 会失败
        let result = mgr
            .execute_download(&model, Path::new("."), Path::new("/nonexistent/python"), &app_config)
            .await;
        assert!(result.is_err());

        cleanup(&dir);
    }

    #[tokio::test]
    async fn test_cancel_download_noop_when_no_download() {
        let dir = temp_dir("cancel_noop");
        let config = test_config(dir.to_str().unwrap());
        let mut mgr = ModelManager::new(&config, Path::new("."));

        // 没有正在进行的下载时，cancel 应该是 no-op，不 panic
        mgr.cancel_download().await;

        cleanup(&dir);
    }
}
