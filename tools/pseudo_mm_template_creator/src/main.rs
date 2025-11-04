//! Pseudo_MM Template Creator Tool
//!
//! Creates a pseudo_mm template from a Firecracker snapshot.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::net::TcpStream;

use clap::{App, Arg};
use serde::Deserialize;
use serde_json;
use snapshot::Snapshot;
use versionize::VersionMap;
use vmm::memory_snapshot::GuestMemoryState;
use vmm::persist::MicrovmState;
use vmm::pseudo_mm_support::{self, PseudoMmTemplate, RegionMetadata, RDMA_MEM};

const DEFAULT_PSEUDO_MM_BASE: u64 = 0x7000_0000_0000;
const PAGE_SIZE: u64 = 4096;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let matches = App::new("Pseudo_MM Template Creator")
        .version("1.0")
        .about("Creates pseudo_mm template from Firecracker snapshot")
        .arg(
            Arg::with_name("snapshot")
                .long("snapshot-path")
                .value_name("FILE")
                .required_unless("batch-config")
                .help("Path to snapshot file"),
        )
        .arg(
            Arg::with_name("mem-file")
                .long("mem-file-path")
                .value_name("FILE")
                .required_unless("batch-config")
                .help("Path to memory file"),
        )
        .arg(
            Arg::with_name("output")
                .long("output-path")
                .value_name("FILE")
                .required_unless("batch-config")
                .help("Output template path"),
        )
        .arg(
            Arg::with_name("rdma-server")
                .long("rdma-server")
                .value_name("ADDR")
                .required_unless("batch-config")
                .help("RDMA control-plane address (host:port)"),
        )
        .arg(
            Arg::with_name("rdma-pgoff")
                .long("rdma-pgoff")
                .value_name("PAGES")
                .required_unless("batch-config")
                .help("Base RDMA page offset to store this snapshot"),
        )
        .arg(
            Arg::with_name("hva-base")
                .long("hva-base")
                .value_name("ADDRESS")
                .help("Base HVA address (hex, default: 0x700000000000)"),
        )
        .arg(
            Arg::with_name("batch-config")
                .long("batch-config")
                .value_name("FILE")
                .conflicts_with("snapshot")
                .help("JSON file describing multiple templates to generate"),
        )
        .get_matches();

    if let Some(config_path) = matches.value_of("batch-config") {
        run_batch(config_path)?;
        return Ok(());
    }

    let snapshot_path = matches.value_of("snapshot").unwrap();
    let mem_file_path = matches.value_of("mem-file").unwrap();
    let output_path = matches.value_of("output").unwrap();
    let rdma_server = matches.value_of("rdma-server").unwrap();
    let rdma_pgoff: u64 = matches
        .value_of("rdma-pgoff")
        .and_then(|s| s.parse().ok())
        .expect("rdma-pgoff must be an unsigned integer");
    let hva_base =
        parse_hex_address(matches.value_of("hva-base")).unwrap_or(DEFAULT_PSEUDO_MM_BASE);

    let result = create_template(&TemplateArgs {
        label: "single",
        snapshot_path,
        mem_file_path,
        output_path,
        rdma_server,
        rdma_pgoff,
        hva_base,
    })?;

    println!("\nSummary:");
    println!("  pseudo_mm_id: {}", result.pseudo_mm_id);
    println!("  rdma_pgoff : {}", result.rdma_pgoff);
    println!("  pages      : {}", result.mem_pages);

    Ok(())
}

fn run_batch(config_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("Loading batch config from {}", config_path);
    let file = File::open(config_path)?;
    let config: BatchConfig = serde_json::from_reader(file)?;

    if config.templates.is_empty() {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidInput,
            "batch config has no templates",
        )));
    }

    let default_rdma_server = config.rdma_server.clone();
    let default_hva_base = parse_optional_hva(config.hva_base.as_deref())?;
    let mut next_rdma_pgoff = config.default_rdma_pgoff.unwrap_or(0);
    let mut summaries = Vec::new();

    println!(
        "Processing {} templates (starting rdma_pgoff={})",
        config.templates.len(),
        next_rdma_pgoff
    );

    for (idx, entry) in config.templates.iter().enumerate() {
        let label = format!("batch-{}", idx + 1);
        let rdma_server = entry
            .rdma_server
            .as_ref()
            .or(default_rdma_server.as_ref())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("template {} missing rdma_server", idx + 1),
                )
            })?;

        let hva_base = entry
            .hva_base
            .as_deref()
            .map(parse_hva_strict)
            .transpose()?;
        let hva_base = hva_base
            .or(default_hva_base)
            .unwrap_or(DEFAULT_PSEUDO_MM_BASE);

        let assigned_pgoff = entry.rdma_pgoff.unwrap_or(next_rdma_pgoff);

        let result = create_template(&TemplateArgs {
            label: &label,
            snapshot_path: &entry.snapshot_path,
            mem_file_path: &entry.mem_file_path,
            output_path: &entry.output_path,
            rdma_server,
            rdma_pgoff: assigned_pgoff,
            hva_base,
        })?;

        let next_candidate = assigned_pgoff + result.mem_pages;
        if entry.rdma_pgoff.is_none() {
            next_rdma_pgoff = next_candidate;
        } else {
            next_rdma_pgoff = std::cmp::max(next_rdma_pgoff, next_candidate);
        }

        summaries.push((label, result));
    }

    println!("\nBatch summary:");
    for (label, summary) in &summaries {
        println!(
            "  [{}] pseudo_mm_id={} rdma_pgoff={} pages={} output={}",
            label, summary.pseudo_mm_id, summary.rdma_pgoff, summary.mem_pages, summary.output_path
        );
    }

    println!("Next available rdma_pgoff: {}", next_rdma_pgoff);

    Ok(())
}

struct TemplateArgs<'a> {
    label: &'a str,
    snapshot_path: &'a str,
    mem_file_path: &'a str,
    output_path: &'a str,
    rdma_server: &'a str,
    rdma_pgoff: u64,
    hva_base: u64,
}

struct TemplateResult {
    pseudo_mm_id: i32,
    rdma_pgoff: u64,
    mem_pages: u64,
    mem_size: u64,
    output_path: String,
}

fn create_template(args: &TemplateArgs) -> Result<TemplateResult, Box<dyn std::error::Error>> {
    println!("\n=== {} :: pseudo_mm template ===", args.label);
    println!("  snapshot : {}", args.snapshot_path);
    println!("  memory   : {}", args.mem_file_path);
    println!("  output   : {}", args.output_path);
    println!("  rdma_srv : {}", args.rdma_server);
    println!("  rdma_off : {}", args.rdma_pgoff);
    println!("  hva_base : 0x{:x}", args.hva_base);

    let guest_memory_state = parse_snapshot(args.snapshot_path)?;
    println!("  regions  : {}", guest_memory_state.regions.len());

    let (mem_size, mem_pages) =
        upload_memory_to_rdma(args.mem_file_path, args.rdma_server, args.rdma_pgoff)?;
    println!("  uploaded : {} bytes ({} pages)", mem_size, mem_pages);

    let pseudo_mm_id = pseudo_mm_support::create_pseudo_mm()?;
    println!("  pseudo_mm: id={}", pseudo_mm_id);

    let mut regions = Vec::new();
    for region in &guest_memory_state.regions {
        let gpa = region.base_address;
        let size = region.size as u64;
        let hva = args.hva_base + gpa;
        if size % PAGE_SIZE != 0 {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "region size 0x{:x} is not page aligned (page size {})",
                    size, PAGE_SIZE
                ),
            )));
        }
        if region.offset % PAGE_SIZE != 0 {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "region offset {} is not page aligned (page size {})",
                    region.offset, PAGE_SIZE
                ),
            )));
        }
        let region_rdma_offset = args.rdma_pgoff + (region.offset / PAGE_SIZE);

        println!(
            "  -> region GPA=0x{:x}, size=0x{:x}, HVA=0x{:x}, RDMA pgoff={}",
            gpa, size, hva, region_rdma_offset
        );

        pseudo_mm_support::add_memory_map(
            pseudo_mm_id,
            hva,
            hva + size,
            (libc::PROT_READ | libc::PROT_WRITE) as u64,
            (libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED) as u64,
            -1,
            0,
        )?;

        pseudo_mm_support::setup_page_table(
            pseudo_mm_id,
            hva,
            size,
            region_rdma_offset,
            RDMA_MEM,
            0,
        )?;

        regions.push(RegionMetadata {
            gpa,
            hva,
            size,
            rdma_offset: region_rdma_offset,
        });
    }

    let template = PseudoMmTemplate {
        pseudo_mm_id,
        hva_base: args.hva_base,
        rdma_base_pgoff: args.rdma_pgoff,
        rdma_image_size: mem_size,
        regions,
    };

    let json = serde_json::to_string_pretty(&template)?;
    std::fs::write(args.output_path, &json)?;
    println!("  saved    : {}", args.output_path);

    Ok(TemplateResult {
        pseudo_mm_id,
        rdma_pgoff: args.rdma_pgoff,
        mem_pages,
        mem_size,
        output_path: args.output_path.to_string(),
    })
}

#[derive(Deserialize)]
struct BatchConfig {
    #[serde(default)]
    rdma_server: Option<String>,
    #[serde(default)]
    default_rdma_pgoff: Option<u64>,
    #[serde(default)]
    hva_base: Option<String>,
    #[serde(default)]
    templates: Vec<BatchTemplateEntry>,
}

#[derive(Deserialize)]
struct BatchTemplateEntry {
    snapshot_path: String,
    mem_file_path: String,
    output_path: String,
    #[serde(default)]
    rdma_pgoff: Option<u64>,
    #[serde(default)]
    rdma_server: Option<String>,
    #[serde(default)]
    hva_base: Option<String>,
}

fn parse_snapshot(path: &str) -> Result<GuestMemoryState, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let version_map = VersionMap::new();
    let microvm_state: MicrovmState = Snapshot::load(&mut reader, version_map).map_err(|err| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("failed to load snapshot: {:?}", err),
        )
    })?;

    Ok(microvm_state.memory_state)
}

fn parse_hex_address(s: Option<&str>) -> Option<u64> {
    s.and_then(|s| {
        let s = s.trim_start_matches("0x");
        u64::from_str_radix(s, 16).ok()
    })
}

fn parse_hva_strict(value: &str) -> Result<u64, Box<dyn std::error::Error>> {
    parse_hex_address(Some(value)).ok_or_else(|| {
        Box::new(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid hva_base '{}': expect hex string", value),
        )) as Box<dyn std::error::Error>
    })
}

fn parse_optional_hva(value: Option<&str>) -> Result<Option<u64>, Box<dyn std::error::Error>> {
    match value {
        None => Ok(None),
        Some(v) => parse_hex_address(Some(v)).map(Some).ok_or_else(|| {
            Box::new(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid hva_base '{}': expect hex string", v),
            )) as Box<dyn std::error::Error>
        }),
    }
}

fn upload_memory_to_rdma(
    mem_file_path: &str,
    rdma_server: &str,
    rdma_pgoff: u64,
) -> Result<(u64, u64), Box<dyn std::error::Error>> {
    let mut file = File::open(mem_file_path)?;
    let size = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(0))?;

    if size % PAGE_SIZE != 0 {
        return Err(Box::new(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "memory snapshot size must be page aligned ({} bytes)",
                PAGE_SIZE
            ),
        )));
    }

    println!(
        "Connecting to RDMA server {} and streaming {} bytes...",
        rdma_server, size
    );
    let mut client = RdmaClient::connect(rdma_server)?;
    client.write_snapshot_from_reader(rdma_pgoff, &mut file, size as u64)?;
    println!("RDMA upload completed");

    Ok((size as u64, (size as u64) / PAGE_SIZE))
}

struct RdmaClient {
    stream: TcpStream,
}

impl RdmaClient {
    fn connect(addr: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let stream = TcpStream::connect(addr)?;
        Ok(Self { stream })
    }

    fn write_snapshot_from_reader(
        &mut self,
        rdma_pgoff: u64,
        reader: &mut File,
        size: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        const CMD_MAP_IMAGE: u32 = 0x1;
        let mut header = [0u8; 24];
        header[0..4].copy_from_slice(&CMD_MAP_IMAGE.to_le_bytes());
        header[8..16].copy_from_slice(&size.to_le_bytes());
        header[16..24].copy_from_slice(&rdma_pgoff.to_le_bytes());
        self.stream.write_all(&header)?;

        let copied = io::copy(reader, &mut self.stream)?;
        if copied != size {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "expected to send {} bytes but only wrote {} bytes",
                    size, copied
                ),
            )));
        }

        let mut ack = [0u8; 4];
        self.stream.read_exact(&mut ack)?;
        let status = i32::from_le_bytes(ack);
        if status != 0 {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::Other,
                format!("RDMA server returned error code {}", status),
            )));
        }
        Ok(())
    }
}
