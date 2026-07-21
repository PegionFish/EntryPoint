use crate::types::{ComputeBackend, ComputeDevice, DeviceId};
use std::process::Command;

use super::DeviceDetector;

pub struct CudaDetector;

const SMI_ARGS: &[&str] = &[
    "--query-gpu=index,name,memory.total,memory.used,utilization.gpu,temperature.gpu",
    "--format=csv,noheader,nounits",
];

fn run_nvidia_smi() -> Option<String> {
    let output = Command::new("nvidia-smi").args(SMI_ARGS).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn parse_mib(s: &str) -> Option<u32> {
    s.trim().parse::<u32>().ok()
}

fn parse_pct(s: &str) -> Option<u8> {
    s.trim().parse::<u8>().ok()
}

pub(crate) fn parse_smi_output(output: &str) -> Vec<ComputeDevice> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(',').map(|f| f.trim()).collect();
            if fields.len() < 6 {
                return None;
            }
            let index: u32 = fields[0].parse().ok()?;
            let name = fields[1].to_string();
            let total_memory_mb = parse_mib(fields[2]);
            let used_memory_mb = parse_mib(fields[3]);
            let utilization = parse_pct(fields[4]);
            let temperature = parse_pct(fields[5]);

            Some(ComputeDevice {
                id: DeviceId::Cuda(index),
                backend: ComputeBackend::Cuda,
                name,
                total_memory_mb,
                used_memory_mb,
                utilization,
                temperature,
            })
        })
        .collect()
}

impl DeviceDetector for CudaDetector {
    fn backend(&self) -> ComputeBackend {
        ComputeBackend::Cuda
    }

    fn detect(&self) -> Vec<ComputeDevice> {
        match run_nvidia_smi() {
            Some(output) => parse_smi_output(&output),
            None => Vec::new(),
        }
    }

    fn refresh(&self, devices: &mut [ComputeDevice]) {
        let Some(output) = run_nvidia_smi() else {
            return;
        };
        let fresh = parse_smi_output(&output);
        for dev in devices.iter_mut() {
            if dev.backend != ComputeBackend::Cuda {
                continue;
            }
            if let Some(updated) = fresh.iter().find(|f| f.id == dev.id) {
                dev.used_memory_mb = updated.used_memory_mb;
                dev.utilization = updated.utilization;
                dev.temperature = updated.temperature;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_gpu() {
        let output = "0, NVIDIA GeForce RTX 4090, 24564, 1234, 45, 62\n";
        let devices = parse_smi_output(output);
        assert_eq!(devices.len(), 1);
        let d = &devices[0];
        assert_eq!(d.id, DeviceId::Cuda(0));
        assert_eq!(d.name, "NVIDIA GeForce RTX 4090");
        assert_eq!(d.total_memory_mb, Some(24564));
        assert_eq!(d.used_memory_mb, Some(1234));
        assert_eq!(d.utilization, Some(45));
        assert_eq!(d.temperature, Some(62));
    }

    #[test]
    fn test_parse_multiple_gpus() {
        let output =
            "0, NVIDIA RTX 4090, 24564, 100, 10, 50\n1, NVIDIA RTX 3080, 10240, 200, 20, 55\n";
        let devices = parse_smi_output(output);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[1].id, DeviceId::Cuda(1));
        assert_eq!(devices[1].total_memory_mb, Some(10240));
    }

    #[test]
    fn test_parse_empty_output() {
        assert!(parse_smi_output("").is_empty());
        assert!(parse_smi_output("\n").is_empty());
    }

    #[test]
    fn test_parse_malformed_line() {
        let output = "garbage line\n0, RTX 4090, 24564, 100, 10, 50\n";
        let devices = parse_smi_output(output);
        assert_eq!(devices.len(), 1);
    }

    #[test]
    fn test_parse_invalid_numbers() {
        let output = "0, RTX 4090, N/A, N/A, N/A, N/A\n";
        let devices = parse_smi_output(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].total_memory_mb, None);
        assert_eq!(devices[0].utilization, None);
    }
}
