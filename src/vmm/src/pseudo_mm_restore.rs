//! Pseudo_MM Restore Module
//!
//! Implements memory restoration using pseudo_mm and RDMA.

use std::fs::File;
use std::io;
use std::path::PathBuf;

use logger::info;
use vm_memory::{GuestAddress, GuestMemoryMmap, GuestRegionMmap, MmapRegion};

use crate::memory_snapshot::Error;
use crate::pseudo_mm_support::{self, PseudoMmTemplate, RegionMetadata};

/// Restore GuestMemoryMmap using pseudo_mm
pub fn restore_with_pseudo_mm(template_path: &PathBuf) -> Result<GuestMemoryMmap, Error> {
    info!("Restoring memory using pseudo_mm from {:?}", template_path);

    // 1. Load template metadata
    let template = load_template(template_path)?;
    info!(
        "Loaded pseudo_mm template: id={}, rdma_base_pgoff={}, size={} bytes, regions={}",
        template.pseudo_mm_id,
        template.rdma_base_pgoff,
        template.rdma_image_size,
        template.regions.len()
    );

    // 2. Attach pseudo_mm to current process
    pseudo_mm_support::attach_to_current_process(template.pseudo_mm_id)
        .map_err(Error::FileHandle)?;
    info!(
        "Attached pseudo_mm id={} to current process",
        template.pseudo_mm_id
    );

    // 3. Create GuestMemoryMmap using existing VMAs
    let mmap_regions = create_guest_regions(&template.regions)?;
    info!("Created {} guest memory regions", mmap_regions.len());

    let guest_memory = GuestMemoryMmap::from_regions(mmap_regions).map_err(Error::CreateMemory)?;

    info!("Pseudo_MM restore completed successfully");

    Ok(guest_memory)
}

/// Load pseudo_mm template from JSON file
fn load_template(path: &PathBuf) -> Result<PseudoMmTemplate, Error> {
    let file = File::open(path).map_err(Error::FileHandle)?;
    let template: PseudoMmTemplate = serde_json::from_reader(file).map_err(|err| {
        Error::FileHandle(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid pseudo_mm template JSON: {}", err),
        ))
    })?;
    Ok(template)
}

/// Create GuestRegionMmap instances from pseudo_mm regions
fn create_guest_regions(regions: &[RegionMetadata]) -> Result<Vec<GuestRegionMmap>, Error> {
    let mut mmap_regions = Vec::new();

    for region in regions {
        // Use the HVA from pseudo_mm (VMA already exists)
        let mmap_region = unsafe {
            MmapRegion::from_raw_ptr(
                region.hva as *mut u8,
                region.size as usize,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        }
        .map_err(Error::CreateRegion)?;

        let guest_region = GuestRegionMmap::new(mmap_region, GuestAddress(region.gpa))
            .map_err(Error::CreateMemory)?;

        mmap_regions.push(guest_region);
    }

    Ok(mmap_regions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires pseudo_mm template file
    fn test_load_template() {
        let path = PathBuf::from("/tmp/test_template.json");
        // Create a dummy template file for testing
        let template = PseudoMmTemplate {
            pseudo_mm_id: 1,
            hva_base: 0x700000000000,
            regions: vec![RegionMetadata {
                gpa: 0,
                hva: 0x700000000000,
                size: 1024 * 1024,
                rdma_offset: 0,
            }],
        };
        let json = serde_json::to_string_pretty(&template).unwrap();
        std::fs::write(&path, json).unwrap();

        let loaded = load_template(&path);
        assert!(loaded.is_ok());
    }
}
