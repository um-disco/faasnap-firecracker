//! Pseudo_MM Support Module
//!
//! Provides low-level ioctl wrappers for pseudo_mm device operations.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::io::{AsRawFd, RawFd};

use serde::{Deserialize, Serialize};

use libc::{c_int, c_ulong};

#[cfg(target_env = "musl")]
type IoctlRequest = c_int;

#[cfg(not(target_env = "musl"))]
type IoctlRequest = c_ulong;

// Pseudo_MM ioctl command numbers (must match definitions in pseudo_mm_ioctl.h)
const PSEUDO_MM_IOC_CREATE: c_ulong = 0x80081c01;
const PSEUDO_MM_IOC_ADD_MAP: c_ulong = 0x40381c03;
const PSEUDO_MM_IOC_SETUP_PT: c_ulong = 0x40301c04;
const PSEUDO_MM_IOC_ATTACH: c_ulong = 0x40081c05;

/// Memory type flag for DAX-backed pseudo_mm mappings.
pub const DAX_MEM: u32 = 0;
/// Memory type flag for RDMA-backed pseudo_mm mappings.
pub const RDMA_MEM: u32 = 1;

/// Pseudo_mm region metadata persisted alongside snapshots.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RegionMetadata {
    /// Guest physical base address associated with this region.
    pub gpa: u64,
    /// Host virtual base address where the region is mapped.
    pub hva: u64,
    /// Region size in bytes (page-aligned).
    pub size: u64,
    /// RDMA page offset encoded in the pseudo_mm page tables.
    pub rdma_offset: u64,
}

/// Aggregate pseudo_mm metadata describing an exported snapshot.
#[derive(Serialize, Deserialize, Debug)]
pub struct PseudoMmTemplate {
    /// Identifier of the pseudo_mm instance created during checkpoint.
    pub pseudo_mm_id: i32,
    /// Base host virtual address used when creating the regions.
    pub hva_base: u64,
    /// Base RDMA page offset used when uploading the memory snapshot.
    pub rdma_base_pgoff: u64,
    /// Size of the uploaded memory snapshot in bytes.
    pub rdma_image_size: u64,
    /// Detailed per-region metadata required for restoration.
    pub regions: Vec<RegionMetadata>,
}

#[repr(C)]
struct PseudoMmAddMapParam {
    id: i32,
    start: u64,
    end: u64,
    prot: u64,
    flags: u64,
    fd: i32,
    offset: i64,
}

#[repr(C)]
struct PseudoMmSetupPtParam {
    id: i32,
    start: u64,
    size: u64,
    pgoff: u64,
    pt_type: u32,
    flags: u64,
}

#[repr(C)]
struct PseudoMmAttachParam {
    pid: i32,
    id: i32,
}

/// Open pseudo_mm device
pub fn open_device() -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/pseudo_mm")
}

/// Create a new pseudo_mm instance
pub fn create_pseudo_mm() -> io::Result<i32> {
    let device = open_device()?;
    let mut pseudo_mm_id: i32 = 0;

    unsafe {
        let ret = libc::ioctl(
            device.as_raw_fd(),
            PSEUDO_MM_IOC_CREATE as IoctlRequest,
            &mut pseudo_mm_id as *mut i32,
        );
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(pseudo_mm_id)
}

/// Add memory mapping to pseudo_mm
pub fn add_memory_map(
    id: i32,
    start: u64,
    end: u64,
    prot: u64,
    flags: u64,
    fd: RawFd,
    offset: i64,
) -> io::Result<()> {
    let device = open_device()?;

    let param = PseudoMmAddMapParam {
        id,
        start,
        end,
        prot,
        flags,
        fd,
        offset,
    };

    unsafe {
        let ret = libc::ioctl(
            device.as_raw_fd(),
            PSEUDO_MM_IOC_ADD_MAP as IoctlRequest,
            &param as *const PseudoMmAddMapParam,
        );
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

/// Setup page table for pseudo_mm region
pub fn setup_page_table(
    id: i32,
    start: u64,
    size: u64,
    pgoff: u64,
    pt_type: u32,
    flags: u64,
) -> io::Result<()> {
    let device = open_device()?;

    let param = PseudoMmSetupPtParam {
        id,
        start,
        size,
        pgoff,
        pt_type,
        flags,
    };

    unsafe {
        let ret = libc::ioctl(
            device.as_raw_fd(),
            PSEUDO_MM_IOC_SETUP_PT as IoctlRequest,
            &param as *const PseudoMmSetupPtParam,
        );
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

/// Attach pseudo_mm to a process
pub fn attach_to_process(pid: i32, id: i32) -> io::Result<()> {
    let device = open_device()?;

    let param = PseudoMmAttachParam { pid, id };

    unsafe {
        let ret = libc::ioctl(
            device.as_raw_fd(),
            PSEUDO_MM_IOC_ATTACH as IoctlRequest,
            &param as *const PseudoMmAttachParam,
        );
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

/// Attach pseudo_mm to current process
pub fn attach_to_current_process(id: i32) -> io::Result<()> {
    let pid = std::process::id() as i32;
    attach_to_process(pid, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires /dev/pseudo_mm device
    fn test_create_pseudo_mm() {
        let result = create_pseudo_mm();
        assert!(result.is_ok());
        let id = result.unwrap();
        assert!(id > 0);
    }
}
