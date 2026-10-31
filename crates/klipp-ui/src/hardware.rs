//! The processor and graphics this machine runs Klipp on, read one way
//! for About, Preferences and the system probe (#3223). Each read them
//! its own way, About not at all, so a screenshot of one could disagree
//! with another, and on Apple Silicon only some knew the chip.
//!
//! The CPU is `/proc/cpuinfo`'s model name, else the chip from the device
//! tree ([`crate::soc_info`]); the GPU is the first display controller
//! `lspci` lists, else the chip's GPU. Both are read once and kept: About
//! and Preferences draw every frame. The driver is the one in use: the
//! shared GPU device's adapter, read as asked, since the device can come
//! up after the first read; without one, the NVIDIA kernel module's
//! version or the chip's driver.

use std::sync::OnceLock;

/// What the machine runs on, as the pages show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hardware {
    /// "AMD Ryzen 9 7950X 16-Core Processor", "Apple M1 (4P + 4E cores)".
    pub cpu: String,
    /// "NVIDIA GeForce RTX 4080", "Apple M1 GPU".
    pub gpu: String,
    /// The graphics in use: "Vulkan, NVIDIA 580.95.05", and the CUDA
    /// version on an NVIDIA card ("…, CUDA 13.0"). `None` when nothing
    /// says.
    pub driver: Option<String>,
}

/// The machine's processor and graphics.
pub fn hardware() -> Hardware {
    static FIXED: OnceLock<(String, String, Option<String>, Option<String>)> = OnceLock::new();
    let (cpu, gpu, fallback, cuda) = FIXED
        .get_or_init(|| {
            let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
            let lspci = std::process::Command::new("lspci").output().ok().and_then(|o| String::from_utf8(o.stdout).ok()).unwrap_or_default();
            let soc = crate::soc_info::gpu();
            let nvidia = std::fs::read_to_string("/proc/driver/nvidia/version").ok().as_deref().and_then(nvidia_version);
            // nvidia-smi only where NVIDIA's module is loaded.
            let cuda = nvidia.as_ref().and(std::process::Command::new("nvidia-smi").output().ok()).and_then(|o| String::from_utf8(o.stdout).ok()).as_deref().and_then(cuda_version);
            let fallback = nvidia.map(|v| format!("NVIDIA {v}")).or_else(|| soc.as_ref().map(|g| g.detail.clone()));
            (cpu_from(&cpuinfo, crate::soc_info::cpu_name), gpu_from(&lspci, soc.map(|g| g.name)), fallback, cuda)
        })
        .clone();
    let in_use = klipp_effects::gpu_context::GpuContext::shared_if_made().and_then(|c| c.adapter_info()).map(driver_from);
    let driver = in_use.or(fallback).map(|d| match &cuda {
        Some(c) => format!("{d}, CUDA {c}"),
        None => d,
    });
    Hardware { cpu, gpu, driver }
}

/// `/proc/cpuinfo`'s first model name, else `soc` (the chip's name).
fn cpu_from(cpuinfo: &str, soc: impl FnOnce() -> Option<String>) -> String {
    cpuinfo
        .lines()
        .find_map(|l| l.strip_prefix("model name"))
        .and_then(|rest| rest.split(':').nth(1))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(soc)
        .unwrap_or_else(|| "Unknown".into())
}

/// The first display controller in `lspci`'s listing, by its maker and
/// model ("NVIDIA GeForce RTX 4080" from "NVIDIA Corporation AD103
/// [GeForce RTX 4080] (rev a1)"), else `soc`.
fn gpu_from(lspci: &str, soc: Option<String>) -> String {
    let line = lspci.lines().find_map(|l| ["VGA compatible controller: ", "3D controller: ", "Display controller: "].iter().find_map(|k| l.split_once(k).map(|(_, rest)| rest.trim())));
    let Some(device) = line else {
        return soc.unwrap_or_else(|| "Unknown".into());
    };
    let device = device.split(" (rev ").next().unwrap_or(device);
    // The model is the last bracket: AMD's names its maker in one first
    // ("Advanced Micro Devices, Inc. [AMD/ATI] Navi 31 [Radeon RX 7900 XTX]").
    let Some(model) = device.rsplit_once('[').and_then(|(_, m)| m.split_once(']')).map(|(m, _)| m.trim()) else {
        return device.to_string();
    };
    let maker = if device.contains("[AMD/ATI]") || device.starts_with("Advanced Micro Devices") {
        "AMD"
    } else {
        device.split_whitespace().next().unwrap_or("")
    };
    if maker.is_empty() || model.starts_with(maker) { model.to_string() } else { format!("{maker} {model}") }
}

/// "Vulkan, NVIDIA 580.95.05": the backend, then the driver and its
/// version as the adapter gives them.
fn driver_from(info: &wgpu::AdapterInfo) -> String {
    let backend = match info.backend {
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Dx12 => "DirectX 12",
        wgpu::Backend::Gl => "OpenGL",
        wgpu::Backend::BrowserWebGpu => "WebGPU",
        wgpu::Backend::Empty => "none",
    };
    let driver = [info.driver.trim(), info.driver_info.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
    if driver.is_empty() { backend.to_string() } else { format!("{backend}, {driver}") }
}

/// The version in the NVIDIA kernel module's first line: the first
/// word of numbers and dots after "Kernel Module" ("NVRM version: NVIDIA
/// UNIX x86_64 Kernel Module  580.95.05  Tue …", and the open module's
/// "… Open Kernel Module for x86_64  615.78.08  Release Build …").
fn nvidia_version(text: &str) -> Option<String> {
    let version = |w: &&str| w.contains('.') && w.chars().all(|c| c.is_ascii_digit() || c == '.');
    text.lines().next()?.split("Kernel Module").nth(1)?.split_whitespace().find(version).map(str::to_string)
}

/// "13.0" from nvidia-smi's banner ("… CUDA Version: 13.0 |").
fn cuda_version(text: &str) -> Option<String> {
    text.split("CUDA Version:").nth(1)?.split_whitespace().next().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cpu_is_cpuinfos_model_else_the_chip() {
        assert_eq!(cpu_from("processor\t: 0\nmodel name\t: AMD Ryzen 9 7950X 16-Core Processor\n", || None), "AMD Ryzen 9 7950X 16-Core Processor");
        assert_eq!(cpu_from("processor\t: 0\nCPU part\t: 0x022\n", || Some("Apple M1 (4P + 4E cores)".into())), "Apple M1 (4P + 4E cores)");
        assert_eq!(cpu_from("", || None), "Unknown");
    }

    #[test]
    fn the_gpu_is_named_by_maker_and_model() {
        let nvidia = "00:00.0 Host bridge: Intel Corporation Device a700\n01:00.0 VGA compatible controller: NVIDIA Corporation AD103 [GeForce RTX 4080] (rev a1)\n";
        assert_eq!(gpu_from(nvidia, None), "NVIDIA GeForce RTX 4080");
        let amd = "03:00.0 VGA compatible controller: Advanced Micro Devices, Inc. [AMD/ATI] Navi 31 [Radeon RX 7900 XTX] (rev c8)\n";
        assert_eq!(gpu_from(amd, None), "AMD Radeon RX 7900 XTX");
        let intel = "00:02.0 VGA compatible controller: Intel Corporation Raptor Lake-P [Iris Xe Graphics] (rev 04)\n";
        assert_eq!(gpu_from(intel, None), "Intel Iris Xe Graphics");
        let compute = "65:00.0 3D controller: NVIDIA Corporation GA100 [A100 PCIe 40GB] (rev a1)\n";
        assert_eq!(gpu_from(compute, None), "NVIDIA A100 PCIe 40GB");
        assert_eq!(gpu_from("", Some("Apple M1 GPU".into())), "Apple M1 GPU", "no PCI display controller: the chip's");
        assert_eq!(gpu_from("", None), "Unknown");
    }

    #[test]
    fn the_driver_is_the_backend_and_the_adapters_driver() {
        let info = |driver: &str, driver_info: &str| wgpu::AdapterInfo {
            name: "NVIDIA GeForce RTX 4080".into(),
            vendor: 0x10de,
            device: 0x2704,
            device_type: wgpu::DeviceType::DiscreteGpu,
            driver: driver.into(),
            driver_info: driver_info.into(),
            backend: wgpu::Backend::Vulkan,
        };
        assert_eq!(driver_from(&info("NVIDIA", "580.95.05")), "Vulkan, NVIDIA 580.95.05");
        assert_eq!(driver_from(&info("", "")), "Vulkan");
        assert_eq!(cuda_version("| NVIDIA-SMI 580.95.05   Driver Version: 580.95.05   CUDA Version: 13.0     |").as_deref(), Some("13.0"));
        assert_eq!(nvidia_version("NVRM version: NVIDIA UNIX x86_64 Kernel Module  580.95.05  Tue Sep 23 09:55:39 UTC 2026\nGCC version: gcc 15").as_deref(), Some("580.95.05"));
        assert_eq!(nvidia_version("NVRM version: NVIDIA UNIX Open Kernel Module for x86_64  615.78.08  Release Build  (root@omarchy)\n").as_deref(), Some("615.78.08"));
    }
}
