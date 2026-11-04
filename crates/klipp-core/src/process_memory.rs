//! How much memory this process holds in RAM, as the kernel counts it.

/// The resident set in MB from `/proc/self/status`; 0 where there is none
/// (not Linux). In kB there, so it is right on any page size: pages are
/// 16 KiB on some ARM kernels.
pub fn resident_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status").ok().and_then(|s| resident_mb_in(&s)).unwrap_or(0.0)
}

/// The `VmRSS:` line of a `status` file's text, in MB.
fn resident_mb_in(status: &str) -> Option<f64> {
    let kb: f64 = status.lines().find_map(|l| l.strip_prefix("VmRSS:"))?.split_whitespace().next()?.parse().ok()?;
    Some(kb / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resident_set_is_read_in_kb_from_status() {
        assert_eq!(resident_mb_in("Name:\tklipp\nVmHWM:\t 9 kB\nVmRSS:\t  2097152 kB\nThreads:\t3\n"), Some(2048.0));
        assert_eq!(resident_mb_in("Name:\tkthread\nThreads:\t1\n"), None);
        assert_eq!(resident_mb_in("VmRSS:\tmany kB\n"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_has_a_resident_set() {
        assert!(resident_mb() > 0.0);
    }
}
