use sysinfo::Disks;

/// Returns (disk_free, disk_total).
///
/// Linux (including containers): root `/` via overlay2 reflects host disk.
/// Windows: picks the largest disk (the physical one).
pub fn disk_space() -> (u64, u64) {
    let disks = Disks::new_with_refreshed_list();

    #[cfg(target_os = "windows")]
    {
        match disks.iter().max_by_key(|d| d.total_space()) {
            Some(d) => (d.available_space(), d.total_space()),
            None => (0, 0),
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        match disks.iter().find(|d| d.mount_point() == std::path::Path::new("/")) {
            Some(d) => (d.available_space(), d.total_space()),
            None => {
                match disks.iter().max_by_key(|d| d.total_space()) {
                    Some(d) => (d.available_space(), d.total_space()),
                    None => (0, 0),
                }
            }
        }
    }
}
