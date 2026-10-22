//! What the drives media is read from can give (#2128): the streams the
//! viewer opens read their files together, and a spinning disk or a
//! network share gives a fraction of what an NVMe drive does. Each file's
//! drive is told by its device, and the device's kind looked up once, from
//! the kernel's block-device and mount tables, then kept: never on a
//! frame's path.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// What kind of drive a file is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriveKind {
    Nvme,
    Ssd,
    Hdd,
    Network,
    /// Its tables could not be read: taken as an SSD.
    Unknown,
}

/// Per kind, the bytes a second the drive is taken to give, and how much
/// earlier than its seek needs a stream on it is opened ahead, so its
/// first frames are read in time. Tuned here, in one place.
pub const DRIVES: [(DriveKind, f64, f64); 5] = [
    (DriveKind::Nvme, 1500e6, 0.0),
    (DriveKind::Ssd, 450e6, 0.0),
    (DriveKind::Hdd, 120e6, 1.0),
    (DriveKind::Network, 40e6, 1.5),
    (DriveKind::Unknown, 450e6, 0.0),
];

impl DriveKind {
    fn row(self) -> (f64, f64) {
        DRIVES.iter().find(|(k, ..)| *k == self).map(|(_, rate, lead)| (*rate, *lead)).unwrap_or((450e6, 0.0))
    }

    /// The bytes a second it is taken to give.
    pub fn read_rate(self) -> f64 {
        self.row().0
    }

    /// The seconds earlier a stream on it is opened ahead.
    pub fn extra_lead_secs(self) -> f64 {
        self.row().1
    }

    pub fn name(self) -> &'static str {
        match self {
            DriveKind::Nvme => "NVMe drive",
            DriveKind::Ssd => "SSD",
            DriveKind::Hdd => "hard disk",
            DriveKind::Network => "network share",
            DriveKind::Unknown => "drive",
        }
    }
}

/// A file's drive: its device, and the device's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drive {
    pub device: u64,
    pub kind: DriveKind,
}

/// The kind of a drive from what its tables say: the filesystem it is
/// mounted as (`fstype`), the block device's name under `/sys/block`, and
/// its `queue/rotational` flag.
pub fn kind_from(fstype: Option<&str>, block_name: Option<&str>, rotational: Option<&str>) -> DriveKind {
    const NETWORK: [&str; 9] = ["nfs", "nfs4", "cifs", "smb3", "smbfs", "fuse.sshfs", "9p", "afs", "ceph"];
    if fstype.is_some_and(|f| NETWORK.contains(&f)) {
        return DriveKind::Network;
    }
    if block_name.is_some_and(|n| n.starts_with("nvme")) {
        return DriveKind::Nvme;
    }
    match rotational.map(str::trim) {
        Some("1") => DriveKind::Hdd,
        Some("0") => DriveKind::Ssd,
        _ => DriveKind::Unknown,
    }
}

/// The filesystem type of the mount `path` is under, and what it was
/// mounted from (`/dev/sda2`, `/dev/mapper/root`, a share's address), from
/// the text of `/proc/self/mountinfo`: the mount whose point is the
/// longest that `path` lies in. By path, not by device: a btrfs
/// subvolume's files have a device number of their own, which no mount
/// line names.
pub fn mount_for_path(mountinfo: &str, path: &Path) -> Option<(String, String)> {
    mountinfo
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let point = fields.get(4)?.replace("\\040", " ");
            let dash = fields.iter().position(|f| *f == "-")?;
            let found = (fields.get(dash + 1)?.to_string(), fields.get(dash + 2).unwrap_or(&"").to_string());
            path.starts_with(&point).then_some((point.len(), found))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, found)| found)
}

/// The kinds told so far, by device, and the drives of the files asked
/// about, by path.
static KINDS: Mutex<Option<HashMap<u64, DriveKind>>> = Mutex::new(None);
static FILES: Mutex<Option<HashMap<std::path::PathBuf, Drive>>> = Mutex::new(None);

/// The drive `path` is read from: looked up the first time the file is
/// asked about, its device's kind the first time the device is seen, and
/// both kept after.
pub fn drive_of(path: &Path) -> Drive {
    if let Some(drive) = FILES.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|f| f.get(path)) {
        return *drive;
    }
    // A file not there (yet) is asked about again when it is.
    let Some(device) = device_of(path) else { return Drive { device: 0, kind: DriveKind::Unknown } };
    let kind = *KINDS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).entry(device).or_insert_with(|| classify(device, path));
    let drive = Drive { device, kind };
    FILES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(path.to_path_buf(), drive);
    drive
}

#[cfg(unix)]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.dev())
}

#[cfg(not(unix))]
fn device_of(_: &Path) -> Option<u64> {
    None
}

/// How many devices have been looked up in the kernel's tables: for the
/// test that a second question about a file reads none of them.
#[cfg(test)]
static LOOKED_UP: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A device number's major and minor, as glibc encodes them.
fn split(dev: u64) -> (u64, u64) {
    (((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0xfff), ((dev >> 12) & 0xffff_ff00) | (dev & 0xff))
}

/// The kind of device `dev`, which `path` is on, from
/// `/proc/self/mountinfo` and `/sys/dev/block`. A filesystem on no block
/// device of its own (btrfs, a mapped or encrypted volume) is followed to
/// the device it was mounted from, and a mapped volume to the disk under
/// it.
#[cfg(target_os = "linux")]
fn classify(dev: u64, path: &Path) -> DriveKind {
    use std::os::unix::fs::MetadataExt;
    #[cfg(test)]
    LOOKED_UP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mount = std::fs::read_to_string("/proc/self/mountinfo").ok().and_then(|m| mount_for_path(&m, &canonical));
    let fstype = mount.as_ref().map(|(fstype, _)| fstype.clone());
    let sys = |(major, minor): (u64, u64)| std::fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")).ok();
    let mut block = sys(split(dev)).or_else(|| {
        let source = mount.as_ref().map(|(_, source)| source.as_str()).filter(|s| s.starts_with("/dev/"))?;
        sys(split(std::fs::metadata(source).ok()?.rdev()))
    });
    // A mapped volume (LUKS, LVM) is the disk under it: its first slave.
    for _ in 0..4 {
        let Some(slave) = block.as_ref().and_then(|b| std::fs::read_dir(b.join("slaves")).ok()?.flatten().next()) else { break };
        block = std::fs::canonicalize(slave.path()).ok();
    }
    // A partition's own folder has no queue: its disk's, one up, does.
    let disk = block.as_ref().map(|b| if b.join("partition").exists() { b.parent().map(Path::to_path_buf).unwrap_or_else(|| b.clone()) } else { b.clone() });
    let name = disk.as_ref().and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned());
    let rotational = disk.and_then(|d| std::fs::read_to_string(d.join("queue/rotational")).ok());
    kind_from(fstype.as_deref(), name.as_deref(), rotational.as_deref())
}

#[cfg(not(target_os = "linux"))]
fn classify(_: u64, _: &Path) -> DriveKind {
    DriveKind::Unknown
}

/// The bytes a second a media file is read at while it plays: its probed
/// bit rates, else its size over its length.
pub fn read_bytes_per_sec(video_kbps: u32, audio_kbps: u32, file_size: u64, duration_secs: f64) -> f64 {
    let kbps = video_kbps as f64 + audio_kbps as f64;
    if kbps > 0.0 {
        kbps * 1000.0 / 8.0
    } else if duration_secs > 0.0 {
        file_size as f64 / duration_secs
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A network filesystem is a share whatever its device; an NVMe name is
    /// NVMe; else the rotational flag tells a disk from an SSD; with none
    /// of it readable, unknown, read as an SSD.
    #[test]
    fn a_drive_is_told_by_its_tables() {
        assert_eq!(kind_from(Some("nfs4"), None, None), DriveKind::Network);
        assert_eq!(kind_from(Some("fuse.sshfs"), Some("sda"), Some("0")), DriveKind::Network);
        assert_eq!(kind_from(Some("ext4"), Some("nvme0n1"), Some("0")), DriveKind::Nvme);
        assert_eq!(kind_from(Some("ext4"), Some("sdb"), Some("1\n")), DriveKind::Hdd);
        assert_eq!(kind_from(Some("btrfs"), Some("sda"), Some("0\n")), DriveKind::Ssd);
        assert_eq!(kind_from(None, None, None), DriveKind::Unknown);
        assert_eq!(DriveKind::Unknown.read_rate(), DriveKind::Ssd.read_rate());
        assert!(DriveKind::Hdd.extra_lead_secs() > 0.0 && DriveKind::Nvme.extra_lead_secs() == 0.0);
    }

    /// The mount a path lies in, and its filesystem type and source, found
    /// in the kernel's mount table by the longest mount point: a btrfs
    /// subvolume's files by their path, whatever their own device.
    #[test]
    fn a_paths_mount_is_found_by_its_point() {
        let mountinfo = "22 1 0:29 /@ / rw,relatime shared:1 - btrfs /dev/mapper/root rw,subvol=/@\n\
                         23 22 0:29 /@home /home rw,relatime shared:2 - btrfs /dev/mapper/root rw,subvol=/@home\n\
                         40 22 0:45 / /mnt/share rw,relatime shared:20 - nfs4 server:/export rw,vers=4.2\n\
                         41 22 8:17 / /media/my\\040disk rw - ext4 /dev/sdb1 rw\n";
        assert_eq!(mount_for_path(mountinfo, Path::new("/home/me/cut/a.mov")), Some(("btrfs".into(), "/dev/mapper/root".into())));
        assert_eq!(mount_for_path(mountinfo, Path::new("/mnt/share/a.mov")), Some(("nfs4".into(), "server:/export".into())));
        assert_eq!(mount_for_path(mountinfo, Path::new("/media/my disk/a.mov")), Some(("ext4".into(), "/dev/sdb1".into())));
        assert_eq!(mount_for_path(mountinfo, Path::new("/mnt/sharelike/a.mov")).map(|m| m.0), Some("btrfs".into()), "a path, not a prefix of a name");
    }

    /// A file's read rate: its bit rates when probed, else its size over
    /// its length.
    #[test]
    fn a_files_read_rate() {
        assert_eq!(read_bytes_per_sec(80_000, 320, 0, 0.0), 80_320.0 * 125.0);
        assert_eq!(read_bytes_per_sec(0, 0, 1_000_000, 10.0), 100_000.0);
        assert_eq!(read_bytes_per_sec(0, 0, 1_000_000, 0.0), 0.0);
    }

    /// A file's drive is looked up once and kept: asked again, no table
    /// is read. A file that isn't there is not kept, so it is looked up
    /// once it is.
    #[test]
    fn a_files_drive_is_kept_and_a_missing_one_is_not() {
        use std::sync::atomic::Ordering;
        let dir = std::env::temp_dir().join(format!("klipp-drive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.mov");
        assert_eq!(drive_of(&file), Drive { device: 0, kind: DriveKind::Unknown }, "not there");
        std::fs::write(&file, b"x").unwrap();
        let first = drive_of(&file);
        assert_ne!(first.device, 0, "looked up once it is there");
        let looked = LOOKED_UP.load(Ordering::Relaxed);
        assert_eq!(drive_of(&file), first);
        assert_eq!(LOOKED_UP.load(Ordering::Relaxed), looked, "asked again: no table read");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
