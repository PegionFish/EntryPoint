//! 公共类型定义 — 所有模块共享的基础类型
//!
//! 此文件由 Wave 0 定义，所有并行 agent 只读引用。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

// ─── 计算后端 ────────────────────────────────────────────────────────────────

/// 计算后端类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComputeBackend {
    /// NVIDIA GPU (CUDA)
    Cuda,
    /// AMD GPU (ROCm)
    Rocm,
    /// Intel CPU/GPU/NPU (OpenVINO)
    OpenVINO,
    /// Windows 通用 GPU 加速 (DirectML)
    DirectML,
    /// 纯 CPU（始终可用）
    Cpu,
}

impl fmt::Display for ComputeBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cuda => write!(f, "cuda"),
            Self::Rocm => write!(f, "rocm"),
            Self::OpenVINO => write!(f, "openvino"),
            Self::DirectML => write!(f, "directml"),
            Self::Cpu => write!(f, "cpu"),
        }
    }
}

impl std::str::FromStr for ComputeBackend {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "cuda" => Ok(Self::Cuda),
            "rocm" => Ok(Self::Rocm),
            "openvino" => Ok(Self::OpenVINO),
            "directml" => Ok(Self::DirectML),
            "cpu" => Ok(Self::Cpu),
            _ => Err(format!("unknown compute backend: {s}")),
        }
    }
}

/// 计算设备标识
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeviceId {
    Cuda(u32),
    Rocm(u32),
    OpenVINO(String),
    DirectML(u32),
    Cpu,
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cuda(i) => write!(f, "cuda:{i}"),
            Self::Rocm(i) => write!(f, "rocm:{i}"),
            Self::OpenVINO(s) => write!(f, "openvino:{s}"),
            Self::DirectML(i) => write!(f, "directml:{i}"),
            Self::Cpu => write!(f, "cpu"),
        }
    }
}

impl DeviceId {
    pub fn backend(&self) -> ComputeBackend {
        match self {
            Self::Cuda(_) => ComputeBackend::Cuda,
            Self::Rocm(_) => ComputeBackend::Rocm,
            Self::OpenVINO(_) => ComputeBackend::OpenVINO,
            Self::DirectML(_) => ComputeBackend::DirectML,
            Self::Cpu => ComputeBackend::Cpu,
        }
    }

    pub fn index(&self) -> Option<u32> {
        match self {
            Self::Cuda(i) | Self::Rocm(i) | Self::DirectML(i) => Some(*i),
            _ => None,
        }
    }
}

/// 物理计算设备信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeDevice {
    pub id: DeviceId,
    pub backend: ComputeBackend,
    pub name: String,
    pub total_memory_mb: Option<u32>,
    pub used_memory_mb: Option<u32>,
    pub utilization: Option<u8>,
    pub temperature: Option<u8>,
}

// ─── 模块分类 ────────────────────────────────────────────────────────────────

/// 模块功能类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModuleCategory {
    Asr,
    Tts,
    Denoise,
    Ocr,
    Image,
    Translate,
    Video,
    Face,
    Custom,
}

impl fmt::Display for ModuleCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Asr => "asr",
            Self::Tts => "tts",
            Self::Denoise => "denoise",
            Self::Ocr => "ocr",
            Self::Image => "image",
            Self::Translate => "translate",
            Self::Video => "video",
            Self::Face => "face",
            Self::Custom => "custom",
        };
        write!(f, "{s}")
    }
}

// ─── 数据类型（管线端口） ────────────────────────────────────────────────────

/// 管线端口数据类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DataType {
    Audio,
    Video,
    Image,
    Text,
    Json,
    File,
}

impl DataType {
    /// 检查 self 是否可以流入 target 类型的端口
    pub fn is_compatible_with(&self, target: &DataType) -> bool {
        if self == target {
            return true;
        }
        matches!(
            (self, target),
            (_, DataType::File)           // 任何文件类型 → file
            | (DataType::Json, DataType::Text) // json → text (序列化)
        )
    }
}

// ─── 服务状态 ────────────────────────────────────────────────────────────────

/// 模块服务运行状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceStatus {
    /// 未安装（缺依赖或模型）
    NotReady,
    /// 已停止
    Stopped,
    /// 正在准备环境/下载模型
    Preparing,
    /// 进程已启动，等待健康检查
    Starting,
    /// 运行中
    Running,
    /// 错误
    Error(String),
}

impl ServiceStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }
}

// ─── 管线任务状态 ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed(String),
    Cancelled,
}

// ─── 通用结果 ────────────────────────────────────────────────────────────────

/// 管线节点输出
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Artifact {
    File(PathBuf),
    Text(String),
    Json(serde_json::Value),
}
