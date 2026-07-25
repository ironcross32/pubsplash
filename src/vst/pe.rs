//! Minimal PE (Portable Executable) inspection: machine architecture and
//! export names, read straight from the file without ever loading it. Used
//! to discard non-VST DLLs and wrong-architecture binaries before the scan
//! helper is asked to actually load anything.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub const MACHINE_X64: u16 = 0x8664;
pub const MACHINE_ARM64: u16 = 0xaa64;

/// The machine type this build of Pubsplash can load plugins for.
pub fn native_machine() -> u16 {
    if cfg!(target_arch = "aarch64") {
        MACHINE_ARM64
    } else {
        MACHINE_X64
    }
}

pub struct PeInfo {
    pub machine: u16,
    pub exports: Vec<String>,
}

impl PeInfo {
    pub fn exports_any(&self, names: &[&str]) -> bool {
        self.exports.iter().any(|e| names.contains(&e.as_str()))
    }
}

/// Caps how many export names we read; VST plugins export a handful.
const MAX_EXPORTS: u32 = 4096;

pub fn inspect(path: &Path) -> Result<PeInfo, String> {
    let mut file = File::open(path).map_err(|e| format!("open failed: {e}"))?;
    let read_at = |file: &mut File, offset: u64, buf: &mut [u8]| -> Result<(), String> {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("seek failed: {e}"))?;
        file.read_exact(buf)
            .map_err(|e| format!("read failed: {e}"))
    };

    let mut dos = [0u8; 64];
    read_at(&mut file, 0, &mut dos)?;
    if &dos[0..2] != b"MZ" {
        return Err("not a PE file (no MZ header)".into());
    }
    let pe_offset = u32::from_le_bytes(dos[60..64].try_into().unwrap()) as u64;

    // PE signature + COFF header.
    let mut coff = [0u8; 24];
    read_at(&mut file, pe_offset, &mut coff)?;
    if &coff[0..4] != b"PE\0\0" {
        return Err("not a PE file (bad signature)".into());
    }
    let machine = u16::from_le_bytes(coff[4..6].try_into().unwrap());
    let num_sections = u16::from_le_bytes(coff[6..8].try_into().unwrap()) as usize;
    let opt_size = u16::from_le_bytes(coff[20..22].try_into().unwrap()) as usize;

    // Optional header: locate the export data directory (entry 0).
    let opt_offset = pe_offset + 24;
    let mut opt = vec![0u8; opt_size];
    read_at(&mut file, opt_offset, &mut opt)?;
    if opt.len() < 2 {
        return Err("optional header missing".into());
    }
    let magic = u16::from_le_bytes(opt[0..2].try_into().unwrap());
    let dir_start = match magic {
        0x010b => 96,  // PE32
        0x020b => 112, // PE32+
        _ => return Err("unknown optional header magic".into()),
    };
    let (export_rva, export_size) = if opt.len() >= dir_start + 8 {
        (
            u32::from_le_bytes(opt[dir_start..dir_start + 4].try_into().unwrap()),
            u32::from_le_bytes(opt[dir_start + 4..dir_start + 8].try_into().unwrap()),
        )
    } else {
        (0, 0)
    };

    // Section table, for RVA -> file offset translation.
    let mut sections = Vec::with_capacity(num_sections);
    let mut section_buf = vec![0u8; 40 * num_sections];
    read_at(&mut file, opt_offset + opt_size as u64, &mut section_buf)?;
    for s in section_buf.chunks_exact(40) {
        let virtual_size = u32::from_le_bytes(s[8..12].try_into().unwrap());
        let virtual_address = u32::from_le_bytes(s[12..16].try_into().unwrap());
        let raw_size = u32::from_le_bytes(s[16..20].try_into().unwrap());
        let raw_ptr = u32::from_le_bytes(s[20..24].try_into().unwrap());
        sections.push((
            virtual_address,
            virtual_size.max(raw_size),
            raw_ptr,
            raw_size,
        ));
    }
    let rva_to_offset = |rva: u32| -> Option<u64> {
        sections
            .iter()
            .find(|(va, vsize, _, _)| rva >= *va && rva < va.saturating_add(*vsize))
            .map(|(va, _, raw_ptr, _)| (rva - va + raw_ptr) as u64)
    };

    let mut exports = Vec::new();
    if export_rva != 0 && export_size != 0 {
        if let Some(dir_offset) = rva_to_offset(export_rva) {
            let mut dir = [0u8; 40];
            read_at(&mut file, dir_offset, &mut dir)?;
            let num_names = u32::from_le_bytes(dir[24..28].try_into().unwrap()).min(MAX_EXPORTS);
            let names_rva = u32::from_le_bytes(dir[32..36].try_into().unwrap());
            if let Some(names_offset) = rva_to_offset(names_rva) {
                let mut name_rvas = vec![0u8; num_names as usize * 4];
                read_at(&mut file, names_offset, &mut name_rvas)?;
                for rva_bytes in name_rvas.chunks_exact(4) {
                    let name_rva = u32::from_le_bytes(rva_bytes.try_into().unwrap());
                    let Some(name_offset) = rva_to_offset(name_rva) else {
                        continue;
                    };
                    // Export names of interest are short; 64 bytes is plenty.
                    let mut name_buf = [0u8; 64];
                    if file.seek(SeekFrom::Start(name_offset)).is_err() {
                        continue;
                    }
                    let read = file.read(&mut name_buf).unwrap_or(0);
                    let end = name_buf[..read]
                        .iter()
                        .position(|&b| b == 0)
                        .unwrap_or(read);
                    if let Ok(name) = std::str::from_utf8(&name_buf[..end]) {
                        exports.push(name.to_string());
                    }
                }
            }
        }
    }

    Ok(PeInfo { machine, exports })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn kernel32() -> PathBuf {
        PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32\\kernel32.dll")
    }

    #[test]
    fn kernel32_is_native_and_exports_createfile() {
        let info = inspect(&kernel32()).unwrap();
        assert_eq!(info.machine, native_machine());
        assert!(info.exports_any(&["CreateFileW"]));
        assert!(!info.exports_any(&["VSTPluginMain", "GetPluginFactory"]));
    }

    #[test]
    fn non_pe_file_is_rejected() {
        let dir = std::env::temp_dir().join("pubsplash-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not_a_pe.dll");
        std::fs::write(&path, b"just some text, definitely not a DLL").unwrap();
        assert!(inspect(&path).is_err());
    }
}
