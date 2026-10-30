//! CPU and GPU names on a machine whose processor and GPU sit on one chip
//! (Apple Silicon under Asahi Linux): read from the device tree, for where
//! the PC sources come up empty. `/proc/cpuinfo` has no `model name` line
//! on arm64, and a GPU on the chip is no PCI device, so `lspci` lists
//! nothing. On x86_64 the PC sources answer and none of this is read.
//!
//! Read once and kept: the About page asks every frame.

use std::sync::OnceLock;

/// The GPU on the chip, as the About page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocGpu {
    /// "Apple M1 GPU".
    pub name: String,
    /// The kernel driver and the GPU generation: "asahi driver, G13G".
    pub detail: String,
}

/// "Apple M1 (4P + 4E cores)", or `None` off Apple Silicon.
pub fn cpu_name() -> Option<String> {
    static NAME: OnceLock<Option<String>> = OnceLock::new();
    NAME.get_or_init(|| {
        let compatible = std::fs::read("/proc/device-tree/compatible").ok()?;
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        cpu_name_from(&compatible, &cpuinfo)
    })
    .clone()
}

/// The first render node's GPU, when it is one on the chip, or `None`.
pub fn gpu() -> Option<SocGpu> {
    static GPU: OnceLock<Option<SocGpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        let mut nodes: Vec<_> = std::fs::read_dir("/sys/class/drm")
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("renderD")))
            .collect();
        nodes.sort();
        nodes.iter().find_map(|node| {
            let compatible = std::fs::read(node.join("device/of_node/compatible")).ok()?;
            let driver = std::fs::read_link(node.join("device/driver")).ok()?;
            gpu_from(&compatible, driver.file_name()?.to_str()?)
        })
    })
    .clone()
}

/// The entries of a device-tree `compatible`: NUL-separated strings.
fn entries(compatible: &[u8]) -> impl Iterator<Item = &str> {
    compatible.split(|&b| b == 0).filter_map(|s| std::str::from_utf8(s).ok()).filter(|s| !s.is_empty())
}

/// The chip's marketing name from its SoC id ("t8103"), or `None` for
/// one not listed here.
fn apple_chip(soc: &str) -> Option<&'static str> {
    Some(match soc {
        "t8103" => "M1",
        "t6000" => "M1 Pro",
        "t6001" => "M1 Max",
        "t6002" => "M1 Ultra",
        "t8112" => "M2",
        "t6020" => "M2 Pro",
        "t6021" => "M2 Max",
        "t6022" => "M2 Ultra",
        _ => return None,
    })
}

/// "Apple M1" for SoC id `t8103`, "Apple Silicon (t9999)" for a chip
/// not in the table.
fn apple_chip_name(soc: &str) -> String {
    match apple_chip(soc) {
        Some(chip) => format!("Apple {chip}"),
        None => format!("Apple Silicon ({soc})"),
    }
}

fn cpu_name_from(compatible: &[u8], cpuinfo: &str) -> Option<String> {
    let soc = entries(compatible).find_map(|e| e.strip_prefix("apple,t").map(|n| format!("t{n}")))?;
    let name = apple_chip_name(&soc);
    // Performance and efficiency cores by `CPU part`, the ids in the
    // kernel's arch/arm64/include/asm/cputype.h: Icestorm and Blizzard
    // (efficiency) are 0x022 0x024 0x028 0x032 0x034 0x038, Firestorm
    // and Avalanche (performance) one above each.
    let (mut p, mut e) = (0, 0);
    for part in cpuinfo.lines().filter_map(|l| l.strip_prefix("CPU part")).filter_map(|l| l.split(':').nth(1)) {
        match part.trim() {
            "0x022" | "0x024" | "0x028" | "0x032" | "0x034" | "0x038" => e += 1,
            "0x023" | "0x025" | "0x029" | "0x033" | "0x035" | "0x039" => p += 1,
            _ => {}
        }
    }
    Some(if p + e > 0 { format!("{name} ({p}P + {e}E cores)") } else { name })
}

fn gpu_from(compatible: &[u8], driver: &str) -> Option<SocGpu> {
    // `apple,agx-t8103 apple,agx-g13g`: the chip, then the GPU generation.
    let mut soc = None;
    let mut generation = None;
    for e in entries(compatible) {
        let Some(agx) = e.strip_prefix("apple,agx-") else { continue };
        if agx.starts_with('t') {
            soc.get_or_insert(agx);
        } else if agx.starts_with('g') {
            generation.get_or_insert(agx);
        }
    }
    let soc = soc?;
    let detail = match generation {
        Some(g) => format!("{driver} driver, {}", g.to_uppercase()),
        None => format!("{driver} driver"),
    };
    Some(SocGpu { name: format!("{} GPU", apple_chip_name(soc)), detail })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Mac mini M1 under Asahi, as its device tree and cpuinfo read.
    #[test]
    fn a_mac_mini_m1_reads_as_an_m1_with_four_and_four_cores() {
        let compatible = b"apple,j274\0apple,t8103\0apple,arm-platform\0";
        let cpuinfo = "processor\t: 0\nCPU part\t: 0x022\n".repeat(4) + &"processor\t: 4\nCPU part\t: 0x023\n".repeat(4);
        assert_eq!(cpu_name_from(compatible, &cpuinfo).as_deref(), Some("Apple M1 (4P + 4E cores)"));
        let gpu = gpu_from(b"apple,agx-t8103\0apple,agx-g13g\0", "asahi").unwrap();
        assert_eq!(gpu, SocGpu { name: "Apple M1 GPU".into(), detail: "asahi driver, G13G".into() });
    }

    #[test]
    fn a_chip_not_in_the_table_is_named_by_its_id() {
        assert_eq!(cpu_name_from(b"apple,t9999\0", "").as_deref(), Some("Apple Silicon (t9999)"));
    }

    /// Not Apple's: the PC sources' "Unknown" stands.
    #[test]
    fn another_board_or_gpu_is_not_named_here() {
        assert_eq!(cpu_name_from(b"raspberrypi,5-model-b\0brcm,bcm2712\0", "CPU part\t: 0xd0b\n"), None);
        assert_eq!(gpu_from(b"brcm,2712-v3d\0", "v3d"), None);
    }
}
