use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use cadio_ir::{CadDocument, CadFormat, CadVersion};
use serde::{Deserialize, Serialize};

mod decode;

const DWG_SENTINEL_LENGTH: usize = 6;
const MIN_TEXT_FRAGMENT_LEN: usize = 4;
const MAX_SUMMARY_FRAGMENTS: usize = 128;
const MAX_PAGE_STRINGS: usize = 32;
const MAX_SECTION_NAMES: usize = 64;
const AC18_HEADER_PRNG_MULTIPLIER: u32 = 0x343FD;
const AC18_HEADER_PRNG_INCREMENT: u32 = 0x269EC3;
const AC18_HEADER_BLOCK_OFFSET: usize = 0x80;
const AC18_HEADER_BLOCK_LEN: usize = 0x6C;
const STRUCTURAL_KEYWORDS: &[&str] = &[
    "LAYER", "BLOCK", "INSERT", "LINE", "POLYLINE", "VERTEX", "ARC", "CIRCLE", "3DFACE", "TEXT",
    "HATCH", "VIEWPORT",
];
const SYSTEM_PAGE_SIGNATURE: u32 = 0x4163_0E3B;
const SECTION_PAGE_MAP_SIGNATURE: u32 = 0x4163_003B;
const SYSTEM_PAGE_HEADER_LEN: usize = 0x14;
const DATA_SECTION_HEADER_LEN: usize = 0x20;
const DATA_SECTION_MASK: u32 = 0x4164_536B;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgProbeSummary {
    pub version: CadVersion,
    pub sentinel: String,
    pub file_size_bytes: u64,
    pub text_fragments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgStructuralSummary {
    pub path: PathBuf,
    pub probe: DwgProbeSummary,
    pub file_header: Option<DwgAc18FileHeaderSummary>,
    pub text_fragment_count: usize,
    pub summary_fragments: Vec<String>,
    pub candidate_layers: Vec<String>,
    pub keyword_hint_counts: BTreeMap<String, usize>,
    pub system_pages: Vec<DwgSystemPageInfo>,
    pub page_map_records: Vec<DwgSectionPageRecord>,
    pub sections: Vec<DwgSectionDescriptorSummary>,
    pub section_names: Vec<String>,
    pub class_count: usize,
    pub class_name_counts: BTreeMap<String, usize>,
    pub class_sample: Vec<DwgClassSummary>,
    pub handle_count: usize,
    pub object_record_count: usize,
    pub object_span_delta_counts: BTreeMap<String, usize>,
    pub object_header_profile_counts: BTreeMap<String, usize>,
    pub object_header_marker_counts: BTreeMap<String, usize>,
    pub object_header_signature_counts: BTreeMap<String, usize>,
    pub short_record_count: usize,
    pub short_record_payload_match_count: usize,
    pub short_record_signature_counts: BTreeMap<String, usize>,
    pub short_payload_signature_counts: BTreeMap<String, usize>,
    pub object_index_sample: Vec<DwgObjectRecordSummary>,
    pub short_record_sample: Vec<DwgShortObjectStub>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgSystemPageInfo {
    pub offset: u64,
    pub kind: DwgSystemPageKind,
    pub decompressor: DwgSystemPageCompression,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub decoded_size: usize,
    pub complete: bool,
    #[serde(skip_serializing, skip_deserializing)]
    pub decoded: Vec<u8>,
    pub strings: Vec<String>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DwgSystemPageKind {
    SectionMap,
    SectionPageMap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DwgSystemPageCompression {
    Uncompressed,
    Compressed,
    Unknown(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgSectionPageRecord {
    pub number: i32,
    pub size: u32,
    pub seeker: u64,
    pub is_gap: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgSectionDescriptorSummary {
    pub section_id: u32,
    pub name: String,
    pub section_size: u64,
    pub page_count: u32,
    pub max_page_size: u32,
    pub compressed_code: u32,
    pub encrypted: u32,
    pub pages: Vec<DwgLocalSectionPageSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgAc18FileHeaderSummary {
    pub file_id: String,
    pub section_page_map_id: u32,
    pub section_map_id: u32,
    pub page_map_address: u64,
    pub section_array_page_size: u32,
    pub gap_array_size: u32,
    pub section_amount: u32,
    pub gap_amount: u32,
    pub last_page_id: u32,
    pub last_section_address: u64,
    pub second_header_address: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgLocalSectionPageSummary {
    pub page_number: u32,
    pub compressed_size: u32,
    pub offset: u64,
    pub decompressed_size: u32,
    pub seeker: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgObjectRecordSummary {
    pub handle: u64,
    pub declared_handle_code: Option<u8>,
    pub declared_handle: Option<u64>,
    pub declared_handle_matches_index: bool,
    pub handle_match_search_delta_bits: Option<u32>,
    pub handle_match_search_encoding: Option<String>,
    pub raw_offset_bits: u64,
    pub raw_offset: u64,
    pub offset_bits: u64,
    pub offset: u64,
    pub span_bytes: usize,
    pub leading_byte: u8,
    pub handle_stream_bits: Option<u64>,
    pub object_type: Option<u32>,
    pub object_type_name: Option<String>,
    pub object_data_bits: Option<u32>,
    pub modular_size_hint: Option<u64>,
    pub modular_size_bytes: Option<usize>,
    pub span_delta_hint: Option<i64>,
    pub header_profile: String,
    pub marker_be_hint: Option<u16>,
    pub flag_hint: Option<u8>,
    pub control_hint: Option<u8>,
    pub header_signature: String,
    pub prefix_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgShortObjectStub {
    pub handle: u64,
    pub header_signature: String,
    pub payload_offset_in_record: usize,
    pub payload_len: usize,
    pub payload_matches_size_hint: bool,
    pub extra_header_byte: Option<u8>,
    pub payload_signature: String,
    pub payload_prefix_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgClassSummary {
    pub class_number: u16,
    pub proxy_flags: u16,
    pub application_name: String,
    pub cpp_class_name: String,
    pub dxf_name: String,
    pub was_zombie: bool,
    pub item_class_id: u16,
    pub instance_count: Option<u32>,
    pub dwg_version_hint: Option<u32>,
    pub maintenance_version_hint: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgRecordProbe {
    pub handle: u64,
    pub raw_offset_bits: u64,
    pub candidate_offset_bits: u64,
    pub object_stream_start_bits: u64,
    pub handle_section_offset_bits: u64,
    pub object_type_name: Option<String>,
    pub self_handle_match_end_bits: Option<u64>,
    pub self_handle_match_end_bits_msb: Option<u64>,
    pub msb_declared_handle: Option<u64>,
    pub lsb_declared_handle: Option<u64>,
    pub object_data_bits: Option<u32>,
    pub handle_stream_bits: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DwgSemanticProbe {
    pub handle: u64,
    pub object_type_name: Option<String>,
    pub decoded_kind: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug)]
pub enum DwgReadError {
    Io(std::io::Error),
    Json(serde_json::Error),
    UnsupportedSignature(String),
    TruncatedHeader,
    MissingSection(String),
    MalformedSection(String),
}

impl std::fmt::Display for DwgReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error while reading DWG: {error}"),
            Self::Json(error) => write!(f, "JSON error while writing DWG summary: {error}"),
            Self::UnsupportedSignature(signature) => {
                write!(f, "Unsupported or invalid DWG signature '{signature}'")
            }
            Self::TruncatedHeader => write!(f, "DWG header is truncated"),
            Self::MissingSection(name) => write!(f, "DWG section '{name}' was not found"),
            Self::MalformedSection(message) => write!(f, "Malformed DWG section: {message}"),
        }
    }
}

impl std::error::Error for DwgReadError {}

impl From<std::io::Error> for DwgReadError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for DwgReadError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub fn probe_file(path: &Path) -> Result<DwgProbeSummary, DwgReadError> {
    let mut file = File::open(path)?;
    let file_size_bytes = file.seek(SeekFrom::End(0))?;
    file.seek(SeekFrom::Start(0))?;
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(file_size_bytes as usize);
    file.read_to_end(&mut bytes)?;
    probe_bytes(&bytes)
}

pub fn read_stub_document(path: &Path) -> Result<CadDocument, DwgReadError> {
    let probe = probe_file(path)?;
    Ok(CadDocument::empty(CadFormat::Dwg, probe.version))
}

pub fn read_document(path: &Path) -> Result<CadDocument, DwgReadError> {
    decode::read_document(path)
}

pub fn probe_record(path: &Path, handle: u64) -> Result<Option<DwgRecordProbe>, DwgReadError> {
    decode::probe_record(path, handle)
}

pub fn probe_semantic_record(
    path: &Path,
    handle: u64,
) -> Result<Option<DwgSemanticProbe>, DwgReadError> {
    decode::probe_semantic_record(path, handle)
}

pub fn summarize_file(path: &Path) -> Result<DwgStructuralSummary, DwgReadError> {
    let probe = probe_file(path)?;
    Ok(summarize_probe(path, probe))
}

pub fn write_summary_json(
    path: &Path,
    output_path: &Path,
) -> Result<DwgStructuralSummary, DwgReadError> {
    let summary = summarize_file(path)?;
    let json = serde_json::to_vec_pretty(&summary)?;
    std::fs::write(output_path, json)?;
    Ok(summary)
}

pub fn read_handle_map(path: &Path) -> Result<BTreeMap<u64, i64>, DwgReadError> {
    let section = read_section_data(path, "Handles")?;
    parse_handle_map(&section)
}

pub fn read_object_index(path: &Path) -> Result<Vec<DwgObjectRecordSummary>, DwgReadError> {
    let handle_map = read_handle_map(path)?;
    let classes = read_classes(path).unwrap_or_default();
    let objects = read_section_data(path, "Objects")?;
    Ok(build_object_index(&handle_map, &objects, &classes))
}

pub fn read_short_object_stubs(path: &Path) -> Result<Vec<DwgShortObjectStub>, DwgReadError> {
    let object_index = read_object_index(path)?;
    let objects = read_section_data(path, "Objects")?;
    Ok(decode_short_object_stubs(&object_index, &objects))
}

pub fn read_classes(path: &Path) -> Result<Vec<DwgClassSummary>, DwgReadError> {
    let probe = probe_file(path)?;
    let section = read_section_data(path, "Classes")?;
    parse_classes_section(&section, probe.version)
}

pub fn read_section_data(path: &Path, section_name: &str) -> Result<Vec<u8>, DwgReadError> {
    let bytes = std::fs::read(path)?;
    let sections = extract_section_descriptors(&bytes);
    read_section_data_from_summary(&bytes, &sections, section_name)
}

fn read_section_data_from_summary(
    bytes: &[u8],
    sections: &[DwgSectionDescriptorSummary],
    section_name: &str,
) -> Result<Vec<u8>, DwgReadError> {
    let expected_name =
        normalize_section_name(section_name).unwrap_or_else(|| section_name.to_string());
    let section = sections
        .iter()
        .find(|section| section.name.eq_ignore_ascii_case(&expected_name))
        .ok_or_else(|| DwgReadError::MissingSection(section_name.to_string()))?;
    let section_size = section.section_size as usize;
    if std::env::var_os("DWG_PROFILE").is_some() && expected_name.contains("bject") {
        eprintln!(
            "dwg section '{}': section_size={} pages={} offsets={:?}",
            section.name,
            section_size,
            section.pages.len(),
            section
                .pages
                .iter()
                .map(|p| (p.page_number, p.offset, p.decompressed_size))
                .collect::<Vec<_>>()
        );
    }
    let report_section =
        std::env::var_os("DWG_PROFILE").is_some() && expected_name.contains("bject");
    let use_offset_placement = section.pages.iter().any(|p| p.offset > 0);
    let mut output = if use_offset_placement {
        vec![0u8; section_size]
    } else {
        Vec::with_capacity(section_size)
    };

    for page in &section.pages {
        let seeker = page.seeker.ok_or_else(|| {
            DwgReadError::MalformedSection(format!(
                "section '{}' page {} has no resolved seeker",
                section.name, page.page_number
            ))
        })? as usize;
        let header = parse_data_section_header(bytes, seeker).ok_or_else(|| {
            DwgReadError::MalformedSection(format!(
                "section '{}' page {} has an invalid encrypted page header",
                section.name, page.page_number
            ))
        })?;
        if header.page_type != 0x4163_043B {
            return Err(DwgReadError::MalformedSection(format!(
                "section '{}' page {} has unexpected page type {:#x}",
                section.name, page.page_number, header.page_type
            )));
        }
        if header.section_number != section.section_id {
            return Err(DwgReadError::MalformedSection(format!(
                "section '{}' page {} resolves to section id {}",
                section.name, page.page_number, header.section_number
            )));
        }
        let payload_start = seeker + DATA_SECTION_HEADER_LEN;
        let payload_end = payload_start
            .checked_add(header.compressed_size as usize)
            .ok_or_else(|| {
                DwgReadError::MalformedSection(format!(
                    "section '{}' page {} has an overflowing payload range",
                    section.name, page.page_number
                ))
            })?;
        let payload = bytes.get(payload_start..payload_end).ok_or_else(|| {
            DwgReadError::MalformedSection(format!(
                "section '{}' page {} payload extends past EOF",
                section.name, page.page_number
            ))
        })?;
        let expected_page_size = page.decompressed_size as usize;
        if report_section {
            eprintln!(
                "dwg section page {}: header.page_size={} page.decompressed_size={} header.compressed_size={} payload_len={} header.start_offset={}",
                page.page_number, header.page_size, page.decompressed_size,
                header.compressed_size, payload.len(), header.start_offset,
            );
        }
        let decode = if section.compressed_code == 2 {
            decode_page_payload(payload, 2, expected_page_size)
        } else {
            DecodeResult {
                output: payload[..payload.len().min(expected_page_size)].to_vec(),
                complete: payload.len() >= expected_page_size,
                warning: None,
            }
        };
        if report_section {
            // Check how many trailing zeros in the decoded output
            let trailing_zeros = decode.output.iter().rev().take_while(|&&b| b == 0).count();
            let last_nonzero = decode.output.len().saturating_sub(trailing_zeros);
            eprintln!(
                "dwg section page {}: decoded_len={} complete={} last_nonzero_byte={} trailing_zeros={}",
                page.page_number, decode.output.len(), decode.complete, last_nonzero, trailing_zeros,
            );
        }
        if !decode.complete {
            return Err(DwgReadError::MalformedSection(format!(
                "section '{}' page {} did not fully decode (decoded {} bytes, page size {}, start {}, checksum {})",
                section.name,
                page.page_number,
                decode.output.len(),
                header.page_size,
                header.start_offset,
                header.checksum
            )));
        }
        let write_len = decode.output.len().min(page.decompressed_size as usize);
        if use_offset_placement {
            let dest_offset = page.offset as usize;
            let dest_end = dest_offset.saturating_add(write_len).min(section_size);
            let copy_len = dest_end.saturating_sub(dest_offset);
            if copy_len > 0 && dest_offset < section_size {
                output[dest_offset..dest_end].copy_from_slice(&decode.output[..copy_len]);
            }
        } else {
            output.extend_from_slice(&decode.output[..write_len]);
        }
    }

    if report_section {
        let first_nonzero = output.iter().position(|b| *b != 0).unwrap_or(0);
        let last_nonzero = output.iter().rposition(|b| *b != 0).unwrap_or(0);
        let zero_count = output.iter().filter(|b| **b == 0).count();
        let zero_runs = {
            let mut runs = Vec::new();
            let mut in_zero = false;
            let mut start = 0usize;
            for (i, &b) in output.iter().enumerate() {
                if b == 0 && !in_zero {
                    in_zero = true;
                    start = i;
                } else if b != 0 && in_zero {
                    let len = i - start;
                    if len >= 256 {
                        runs.push((start, len));
                    }
                    in_zero = false;
                }
            }
            if in_zero && output.len() - start >= 256 {
                runs.push((start, output.len() - start));
            }
            runs
        };
        eprintln!(
            "dwg section Objects: {} bytes, {} zeros, first_nonzero={}, last_nonzero={}, zero_runs(>=256)={:?}",
            output.len(), zero_count, first_nonzero, last_nonzero, zero_runs
        );
    }
    if !use_offset_placement {
        if output.len() < section_size {
            output.resize(section_size, 0);
        } else if output.len() > section_size {
            output.truncate(section_size);
        }
    }

    Ok(output)
}

fn summarize_probe(path: &Path, probe: DwgProbeSummary) -> DwgStructuralSummary {
    let file_bytes = std::fs::read(path).ok();
    let system_pages = file_bytes
        .as_deref()
        .map(scan_system_pages_from_bytes)
        .unwrap_or_default();
    let file_header = file_bytes
        .as_deref()
        .and_then(|bytes| parse_ac18_file_header(bytes, probe.version));
    let page_map_records = system_pages
        .iter()
        .find(|page| page.kind == DwgSystemPageKind::SectionPageMap)
        .map(|page| parse_section_page_records(&page.decoded))
        .unwrap_or_default();
    let sections = system_pages
        .iter()
        .find(|page| page.kind == DwgSystemPageKind::SectionMap)
        .map(|page| parse_section_descriptors(&page.decoded, &page_map_records))
        .unwrap_or_default();
    let text_fragment_count = probe.text_fragments.len();
    let summary_fragments = probe
        .text_fragments
        .iter()
        .take(MAX_SUMMARY_FRAGMENTS)
        .cloned()
        .collect();
    let handle_map = file_bytes
        .as_deref()
        .and_then(|bytes| read_section_data_from_summary(bytes, &sections, "Handles").ok())
        .and_then(|handles| parse_handle_map(&handles).ok())
        .unwrap_or_default();
    let classes = file_bytes
        .as_deref()
        .and_then(|bytes| read_section_data_from_summary(bytes, &sections, "Classes").ok())
        .and_then(|classes| parse_classes_section(&classes, probe.version).ok())
        .unwrap_or_default();
    let object_index = file_bytes
        .as_deref()
        .and_then(|bytes| read_section_data_from_summary(bytes, &sections, "Objects").ok())
        .map(|objects| build_object_index(&handle_map, &objects, &classes))
        .unwrap_or_default();
    let short_record_sample = file_bytes
        .as_deref()
        .and_then(|bytes| read_section_data_from_summary(bytes, &sections, "Objects").ok())
        .map(|objects| decode_short_object_stubs(&object_index, &objects))
        .unwrap_or_default();
    DwgStructuralSummary {
        path: path.to_path_buf(),
        file_header,
        text_fragment_count,
        summary_fragments,
        candidate_layers: infer_candidate_layers(&probe.text_fragments),
        keyword_hint_counts: keyword_hint_counts(&probe.text_fragments),
        page_map_records,
        section_names: infer_section_names(&system_pages, &sections),
        sections,
        system_pages,
        class_count: classes.len(),
        class_name_counts: class_name_counts(&classes),
        class_sample: classes.into_iter().take(32).collect(),
        handle_count: handle_map.len(),
        object_record_count: object_index.len(),
        object_span_delta_counts: object_span_delta_counts(&object_index),
        object_header_profile_counts: object_header_profile_counts(&object_index),
        object_header_marker_counts: object_header_marker_counts(&object_index),
        object_header_signature_counts: object_header_signature_counts(&object_index),
        short_record_count: short_record_sample.len(),
        short_record_payload_match_count: short_record_sample
            .iter()
            .filter(|record| record.payload_matches_size_hint)
            .count(),
        short_record_signature_counts: short_record_signature_counts(&short_record_sample),
        short_payload_signature_counts: short_payload_signature_counts(&short_record_sample),
        object_index_sample: object_index.into_iter().take(64).collect(),
        short_record_sample: short_record_sample.into_iter().take(64).collect(),
        probe,
    }
}

fn probe_bytes(bytes: &[u8]) -> Result<DwgProbeSummary, DwgReadError> {
    let sentinel = bytes
        .get(..DWG_SENTINEL_LENGTH)
        .ok_or(DwgReadError::TruncatedHeader)?;
    let sentinel = String::from_utf8_lossy(sentinel).to_string();
    let version = parse_version_sentinel(&sentinel)
        .ok_or_else(|| DwgReadError::UnsupportedSignature(sentinel.clone()))?;
    Ok(DwgProbeSummary {
        version,
        sentinel,
        file_size_bytes: bytes.len() as u64,
        text_fragments: extract_text_fragments(bytes),
    })
}

fn parse_version_sentinel(sentinel: &str) -> Option<CadVersion> {
    match sentinel {
        "AC1012" => Some(CadVersion::AcadR13),
        "AC1014" => Some(CadVersion::AcadR14),
        "AC1015" => Some(CadVersion::Acad2000),
        "AC1018" => Some(CadVersion::Acad2004),
        "AC1021" => Some(CadVersion::Acad2007),
        "AC1024" => Some(CadVersion::Acad2010),
        "AC1027" => Some(CadVersion::Acad2013),
        "AC1032" => Some(CadVersion::Acad2018),
        _ => None,
    }
}

fn parse_ac18_file_header(bytes: &[u8], version: CadVersion) -> Option<DwgAc18FileHeaderSummary> {
    if !matches!(
        version,
        CadVersion::Acad2004
            | CadVersion::Acad2007
            | CadVersion::Acad2010
            | CadVersion::Acad2013
            | CadVersion::Acad2018
    ) {
        return None;
    }
    let encoded =
        bytes.get(AC18_HEADER_BLOCK_OFFSET..AC18_HEADER_BLOCK_OFFSET + AC18_HEADER_BLOCK_LEN)?;
    let decrypted = decrypt_ac18_header_block(encoded);
    let file_id = String::from_utf8_lossy(decrypted.get(..12)?)
        .trim_end_matches('\0')
        .to_string();
    if !file_id.starts_with("AcFssFcAJMB") {
        return None;
    }
    Some(DwgAc18FileHeaderSummary {
        file_id,
        gap_amount: u32_from_slice(&decrypted, 0x3C)?,
        section_amount: u32_from_slice(&decrypted, 0x40)?,
        section_page_map_id: u32_from_slice(&decrypted, 0x50)?,
        page_map_address: u64_from_slice(&decrypted, 0x54)?.saturating_add(0x100),
        section_map_id: u32_from_slice(&decrypted, 0x5C)?,
        section_array_page_size: u32_from_slice(&decrypted, 0x60)?,
        gap_array_size: u32_from_slice(&decrypted, 0x64)?,
        last_page_id: u32_from_slice(&decrypted, 0x28)?,
        last_section_address: u64_from_slice(&decrypted, 0x2C)?,
        second_header_address: u64_from_slice(&decrypted, 0x34)?,
    })
}

fn decrypt_ac18_header_block(encoded: &[u8]) -> Vec<u8> {
    let mut rand_seed = 1_u32;
    encoded
        .iter()
        .map(|value| {
            rand_seed = rand_seed
                .wrapping_mul(AC18_HEADER_PRNG_MULTIPLIER)
                .wrapping_add(AC18_HEADER_PRNG_INCREMENT);
            value ^ ((rand_seed >> 16) as u8)
        })
        .collect()
}

fn parse_data_section_header(bytes: &[u8], offset: usize) -> Option<DwgDataSectionHeader> {
    let mask = DATA_SECTION_MASK ^ offset as u32;
    let page_type = u32_from_slice(bytes, offset)? ^ mask;
    let section_number = u32_from_slice(bytes, offset + 4)? ^ mask;
    let compressed_size = u32_from_slice(bytes, offset + 8)? ^ mask;
    let page_size = u32_from_slice(bytes, offset + 12)? ^ mask;
    let start_offset = u32_from_slice(bytes, offset + 16)? ^ mask;
    let checksum = u32_from_slice(bytes, offset + 20)? ^ mask;
    Some(DwgDataSectionHeader {
        page_type,
        section_number,
        compressed_size,
        page_size,
        start_offset,
        checksum,
    })
}

fn parse_handle_map(section: &[u8]) -> Result<BTreeMap<u64, i64>, DwgReadError> {
    let profile = std::env::var_os("DWG_PROFILE").is_some();
    if profile {
        let preview: Vec<u8> = section.iter().take(32).copied().collect();
        eprintln!(
            "dwg handle_map: section_len={} preview={:02x?}",
            section.len(),
            preview
        );
    }
    let mut map = BTreeMap::new();
    let mut index = 0usize;
    let mut block_count = 0usize;
    while index + 2 <= section.len() {
        let size = u16::from_be_bytes(section[index..index + 2].try_into().map_err(|_| {
            DwgReadError::MalformedSection("invalid handle section size".to_string())
        })?) as usize;
        if size == 0 {
            break;
        }
        if size < 2 {
            return Err(DwgReadError::MalformedSection(
                "handle section block shorter than CRC trailer".to_string(),
            ));
        }
        index += 2;
        if size == 0 {
            break;
        }
        let start = index;
        let data_size = size.saturating_sub(2);
        let end = start.checked_add(data_size).ok_or_else(|| {
            DwgReadError::MalformedSection(
                "handle section block overflowed reconstructed buffer".to_string(),
            )
        })?;
        if start + size > section.len() {
            return Err(DwgReadError::MalformedSection(
                "handle section extends past reconstructed Handles buffer".to_string(),
            ));
        }
        if profile && block_count < 3 {
            let preview: Vec<u8> = section
                .get(start..start.saturating_add(20).min(section.len()))
                .unwrap_or(&[])
                .to_vec();
            eprintln!(
                "dwg handle_map: block[{}] size={} start={} end={} preview={:02x?}",
                block_count, size, start, end, preview
            );
        }
        block_count += 1;
        let mut last_handle = 0_u64;
        let mut last_location = 0_i64;
        let mut entry_count = 0usize;
        while index + 1 < end {
            let offset = read_modular_char(section, &mut index)?;
            if index >= end {
                break;
            }
            last_handle = last_handle.saturating_add(offset);
            last_location =
                last_location.saturating_add(read_signed_modular_char(section, &mut index)?);
            if offset > 0 {
                map.insert(last_handle, last_location);
            }
            if profile && entry_count < 5 {
                eprintln!(
                    "dwg handle_map: entry[{}] delta_handle={} handle={:#x} loc={}",
                    entry_count, offset, last_handle, last_location
                );
            }
            entry_count += 1;
        }
        index = end;
        if profile && (block_count <= 3 || index != end) {
            eprintln!(
                "dwg handle_map: block[{}] done: {} entries, index={} end={} overshoot={}",
                block_count - 1,
                entry_count,
                index,
                end,
                index as isize - end as isize
            );
        }
        if index + 2 > section.len() {
            return Err(DwgReadError::MalformedSection(
                "handle section CRC extends past reconstructed Handles buffer".to_string(),
            ));
        }
        index += 2;
    }
    Ok(map)
}

fn build_object_index(
    handle_map: &BTreeMap<u64, i64>,
    objects: &[u8],
    classes: &[DwgClassSummary],
) -> Vec<DwgObjectRecordSummary> {
    let mut raw_records = handle_map
        .iter()
        .filter_map(|(handle, raw_offset_bytes)| {
            let raw_offset_bytes = u64::try_from(*raw_offset_bytes).ok()?;
            let raw_offset = usize::try_from(raw_offset_bytes).ok()?;
            let raw_offset_bits = raw_offset_bytes.checked_mul(8)?;
            let bit_index = usize::try_from(raw_offset_bits).ok()?;
            let _ = objects.get(raw_offset..)?;
            Some((
                *handle,
                raw_offset_bytes,
                raw_offset_bits,
                raw_offset,
                bit_index,
            ))
        })
        .collect::<Vec<_>>();
    raw_records.sort_by_key(|(_, raw_offset_bytes, raw_offset_bits, raw_offset, _)| {
        (*raw_offset_bytes, *raw_offset_bits, *raw_offset)
    });

    let mut records = raw_records
        .iter()
        .enumerate()
        .filter_map(
            |(record_index, (handle, raw_offset_bytes, raw_offset_bits, raw_offset, bit_index))| {
                let next_raw_offset_bits = raw_records
                    .get(record_index + 1)
                    .and_then(|(_, next_raw_offset_bytes, _, _, _)| {
                        next_raw_offset_bytes.checked_mul(8)
                    })
                    .unwrap_or_else(|| {
                        u64::try_from(objects.len())
                            .ok()
                            .map(|len| len * 8)
                            .unwrap_or(0)
                    });
                let modern_start = find_modern_object_start_candidate(
                    objects,
                    *handle,
                    *raw_offset_bits,
                    next_raw_offset_bits,
                    classes,
                );
                let record_start_bits = modern_start
                    .as_ref()
                    .map(|candidate| candidate.record_start_bits)
                    .unwrap_or(*bit_index);
                let record_start = modern_start
                    .as_ref()
                    .map(|candidate| candidate.record_start_bits / 8)
                    .unwrap_or(*raw_offset);
                let leading_byte = read_stream_u8(objects, record_start_bits)?;
                let parsed_header = parse_object_header(
                    objects,
                    modern_start
                        .as_ref()
                        .map(|candidate| candidate.object_stream_start_bits)
                        .unwrap_or(record_start_bits),
                );
                let matched_handle = parsed_header.and_then(|header| {
                    search_for_object_handle(objects, header.post_header_bit_index, *handle)
                });
                let prefix = parse_object_prefix(objects, record_start_bits);
                let prefix_hex = bitstream_hex_preview(objects, record_start_bits, 12);
                Some((
                    *handle,
                    *raw_offset_bytes,
                    *raw_offset_bits,
                    *raw_offset,
                    u64::try_from(record_start_bits).ok()?,
                    record_start,
                    leading_byte,
                    parsed_header,
                    matched_handle,
                    prefix,
                    prefix_hex,
                    modern_start,
                ))
            },
        )
        .collect::<Vec<_>>();
    records.sort_by_key(
        |(
            _handle,
            _raw_offset_bytes,
            _raw_offset_bits,
            _raw_offset,
            offset_bits,
            offset,
            _leading_byte,
            _parsed_header,
            _matched_handle,
            _prefix,
            _prefix_hex,
            _modern_start,
        )| (*offset_bits, *offset),
    );

    let mut next_distinct_offsets = vec![objects.len(); records.len()];
    let mut next_offset_bits = u64::try_from(objects.len())
        .ok()
        .map(|len| len * 8)
        .unwrap_or(0);
    for record_index in (0..records.len()).rev() {
        let current_offset_bits = records[record_index].2;
        let current_offset = records[record_index].3;
        let next_offset = usize::try_from(next_offset_bits / 8).unwrap_or(objects.len());
        next_distinct_offsets[record_index] = next_offset;
        if current_offset_bits < next_offset_bits {
            next_offset_bits = current_offset_bits;
        } else if current_offset < next_offset {
            next_offset_bits = u64::try_from(current_offset)
                .ok()
                .map(|value| value * 8)
                .unwrap_or(next_offset_bits);
        }
    }

    records
        .into_iter()
        .zip(next_distinct_offsets)
        .map(
            |(
                (
                    handle,
                    _raw_offset_bytes,
                    raw_offset_bits,
                    raw_offset,
                    offset_bits,
                    offset,
                    leading_byte,
                    parsed_header,
                    matched_handle,
                    prefix,
                    prefix_hex,
                    modern_start,
                ),
                next_offset,
            )| {
                let span_bytes = next_offset.saturating_sub(offset);
                let object_type = modern_start
                    .as_ref()
                    .map(|candidate| candidate.object_type)
                    .or_else(|| parsed_header.map(|header| header.object_type));
                let record_span_bits = u64::try_from(span_bytes).ok().map(|span| span * 8);
                let handle_stream_bits = modern_start
                    .as_ref()
                    .map(|candidate| candidate.handle_stream_bits)
                    .or_else(|| parsed_header.map(|header| header.handle_stream_bits))
                    .filter(|bits| record_span_bits.is_some_and(|span| *bits <= span));
                let object_data_bits = parsed_header
                    .map(|header| header.object_data_bits)
                    .filter(|bits| record_span_bits.is_some_and(|span| u64::from(*bits) <= span));
                let modular_size_hint = modern_start
                    .as_ref()
                    .and_then(|candidate| u64::try_from(candidate.size_bytes).ok())
                    .or(prefix.modular_size_hint);
                let modular_size_bytes = modern_start
                    .as_ref()
                    .map(|candidate| candidate.size_field_bytes)
                    .or(prefix.modular_size_bytes);
                let header_profile =
                    object_header_profile(modular_size_hint, modular_size_bytes, span_bytes);
                DwgObjectRecordSummary {
                    handle,
                    declared_handle_code: parsed_header.map(|header| header.declared_handle_code),
                    declared_handle: parsed_header.map(|header| header.declared_handle),
                    declared_handle_matches_index: parsed_header
                        .is_some_and(|header| header.declared_handle == handle),
                    handle_match_search_delta_bits: matched_handle
                        .map(|match_info| match_info.delta_bits),
                    handle_match_search_encoding: matched_handle
                        .map(|match_info| match_info.encoding.to_string()),
                    raw_offset_bits,
                    raw_offset: raw_offset as u64,
                    offset_bits,
                    offset: offset as u64,
                    span_bytes,
                    leading_byte,
                    handle_stream_bits,
                    object_type,
                    object_type_name: object_type
                        .and_then(|object_type| object_type_name(object_type, classes)),
                    object_data_bits,
                    modular_size_hint,
                    modular_size_bytes,
                    span_delta_hint: modular_size_hint.and_then(|size_hint| {
                        i64::try_from(span_bytes)
                            .ok()
                            .map(|span| span - size_hint as i64)
                    }),
                    header_signature: object_header_signature(
                        &header_profile,
                        prefix.marker_be_hint,
                        prefix.flag_hint,
                        prefix.control_hint,
                    ),
                    header_profile,
                    marker_be_hint: prefix.marker_be_hint,
                    flag_hint: prefix.flag_hint,
                    control_hint: prefix.control_hint,
                    prefix_hex,
                }
            },
        )
        .collect()
}

fn parse_classes_section(
    section: &[u8],
    version: CadVersion,
) -> Result<Vec<DwgClassSummary>, DwgReadError> {
    const SENTINEL_LEN: usize = 16;
    const AC24_UNKNOWN_SIZE_LEN: usize = 4;
    if section.len() < SENTINEL_LEN + 4 + SENTINEL_LEN {
        return Err(DwgReadError::MalformedSection(
            "Classes section shorter than sentinel/size framing".to_string(),
        ));
    }

    let mut offset = SENTINEL_LEN;
    let _class_data_size = u32_from_slice(section, offset).ok_or_else(|| {
        DwgReadError::MalformedSection("Classes section is missing class data size".to_string())
    })?;
    offset += 4;

    if matches!(
        version,
        CadVersion::Acad2010 | CadVersion::Acad2013 | CadVersion::Acad2018
    ) {
        if section.len() < offset + AC24_UNKNOWN_SIZE_LEN {
            return Err(DwgReadError::MalformedSection(
                "Classes section is missing the AC24 size high word".to_string(),
            ));
        }
        offset += AC24_UNKNOWN_SIZE_LEN;
    }

    if section.len() < offset + 4 {
        return Err(DwgReadError::MalformedSection(
            "Classes section is missing merged-stream framing".to_string(),
        ));
    }

    let merged_size_anchor_bit = offset * 8;
    let merged_size_bits = u32_from_slice(section, offset).ok_or_else(|| {
        DwgReadError::MalformedSection("Classes merged stream is missing total size".to_string())
    })? as usize;
    offset += 4;
    let main_start_bit = offset * 8;
    let flag_pos_bit = merged_size_anchor_bit
        .checked_add(merged_size_bits)
        .and_then(|value| value.checked_sub(1))
        .ok_or_else(|| {
            DwgReadError::MalformedSection("Classes merged size overflowed".to_string())
        })?;
    let text_start_bit = locate_embedded_text_stream(section, flag_pos_bit)?;

    let mut main_reader = MsbBitReader::new(section);
    main_reader.bit_index = main_start_bit;
    let _ = read_bitlong_msb(&mut main_reader).ok_or_else(|| {
        DwgReadError::MalformedSection("Classes merged preamble is missing BL 0 marker".to_string())
    })?;
    let _ = main_reader.read_bit().ok_or_else(|| {
        DwgReadError::MalformedSection(
            "Classes merged preamble is missing string flag bit".to_string(),
        )
    })?;

    let mut text_reader = MsbBitReader::new(section);
    text_reader.bit_index = text_start_bit;

    let mut classes = Vec::new();
    while main_reader.bit_index < text_start_bit {
        let Some(class_number) = read_bitshort(&mut main_reader) else {
            break;
        };
        let Some(proxy_flags) = read_bitshort(&mut main_reader) else {
            break;
        };
        let application_name = read_variable_text(&mut text_reader, version)?;
        let cpp_class_name = read_variable_text(&mut text_reader, version)?;
        let dxf_name = read_variable_text(&mut text_reader, version)?;
        let was_zombie = main_reader.read_bit().ok_or_else(|| {
            DwgReadError::MalformedSection(
                "Classes section ended while reading was_zombie".to_string(),
            )
        })?;
        let item_class_id = read_bitshort(&mut main_reader).ok_or_else(|| {
            DwgReadError::MalformedSection(
                "Classes section ended while reading item_class_id".to_string(),
            )
        })?;
        let instance_count = if is_r2004_plus(version) {
            Some(read_bitlong_msb(&mut main_reader).ok_or_else(|| {
                DwgReadError::MalformedSection(
                    "Classes section ended while reading instance_count".to_string(),
                )
            })?)
        } else {
            None
        };
        let dwg_version_hint = if is_r2004_plus(version) {
            Some(read_bitlong_msb(&mut main_reader).ok_or_else(|| {
                DwgReadError::MalformedSection(
                    "Classes section ended while reading class dwg version".to_string(),
                )
            })?)
        } else {
            None
        };
        let maintenance_version_hint = if is_r2004_plus(version) {
            Some(read_bitlong_msb(&mut main_reader).ok_or_else(|| {
                DwgReadError::MalformedSection(
                    "Classes section ended while reading class maintenance version".to_string(),
                )
            })?)
        } else {
            None
        };
        if is_r2004_plus(version) {
            let _ = read_bitlong_msb(&mut main_reader);
            let _ = read_bitlong_msb(&mut main_reader);
        }

        classes.push(DwgClassSummary {
            class_number,
            proxy_flags,
            application_name,
            cpp_class_name,
            dxf_name,
            was_zombie,
            item_class_id,
            instance_count,
            dwg_version_hint,
            maintenance_version_hint,
        });
    }

    Ok(classes)
}

fn locate_embedded_text_stream(section: &[u8], flag_pos_bit: usize) -> Result<usize, DwgReadError> {
    let mut flag_reader = MsbBitReader::new(section);
    flag_reader.bit_index = flag_pos_bit;
    let has_text_stream = flag_reader.read_bit().ok_or_else(|| {
        DwgReadError::MalformedSection("Classes flag position is outside the section".to_string())
    })?;
    if !has_text_stream {
        return Ok(flag_pos_bit);
    }

    let mut length_bit = flag_pos_bit.checked_sub(16).ok_or_else(|| {
        DwgReadError::MalformedSection("Classes text flag underflowed".to_string())
    })?;
    let mut size_reader = MsbBitReader::new(section);
    size_reader.bit_index = length_bit;
    let mut string_data_size = usize::from(size_reader.read_u16_le().ok_or_else(|| {
        DwgReadError::MalformedSection("Classes text stream is missing low size word".to_string())
    })?);
    if (string_data_size & 0x8000) != 0 {
        length_bit = length_bit.checked_sub(16).ok_or_else(|| {
            DwgReadError::MalformedSection("Classes text stream high size underflowed".to_string())
        })?;
        let mut hi_reader = MsbBitReader::new(section);
        hi_reader.bit_index = length_bit;
        let hi_size = usize::from(hi_reader.read_u16_le().ok_or_else(|| {
            DwgReadError::MalformedSection(
                "Classes text stream is missing high size word".to_string(),
            )
        })?);
        string_data_size = (string_data_size & 0x7FFF) | (hi_size << 15);
    }

    length_bit.checked_sub(string_data_size).ok_or_else(|| {
        DwgReadError::MalformedSection("Classes text stream start underflowed".to_string())
    })
}

fn read_bitshort(reader: &mut MsbBitReader<'_>) -> Option<u16> {
    match reader.read_bits(2)? {
        0b00 => reader.read_u16_le(),
        0b01 => Some(u16::from(reader.read_u8()?)),
        0b10 => Some(0),
        0b11 => Some(256),
        _ => None,
    }
}

fn read_variable_text(
    reader: &mut MsbBitReader<'_>,
    version: CadVersion,
) -> Result<String, DwgReadError> {
    let text_length = usize::from(read_bitshort(reader).ok_or_else(|| {
        DwgReadError::MalformedSection("Text stream ended while reading string length".to_string())
    })?);
    if text_length == 0 {
        return Ok(String::new());
    }
    let byte_len = if is_r2007_plus(version) {
        text_length.checked_mul(2).ok_or_else(|| {
            DwgReadError::MalformedSection("Text stream string length overflowed".to_string())
        })?
    } else {
        text_length
    };
    let mut bytes = Vec::with_capacity(byte_len);
    for _ in 0..byte_len {
        bytes.push(reader.read_u8().ok_or_else(|| {
            DwgReadError::MalformedSection(
                "Text stream ended while reading string bytes".to_string(),
            )
        })?);
    }
    if is_r2007_plus(version) {
        let mut units = Vec::with_capacity(text_length);
        for chunk in bytes.chunks_exact(2) {
            units.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        Ok(String::from_utf16_lossy(&units).replace('\0', ""))
    } else {
        Ok(String::from_utf8_lossy(&bytes)
            .replace('\0', "")
            .to_string())
    }
}

fn class_name_counts(classes: &[DwgClassSummary]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for class in classes {
        let key = if !class.dxf_name.is_empty() {
            class.dxf_name.clone()
        } else if !class.cpp_class_name.is_empty() {
            class.cpp_class_name.clone()
        } else {
            format!("class#{}", class.class_number)
        };
        *counts.entry(key).or_insert(0) += 1;
    }
    counts
}

fn is_r2004_plus(version: CadVersion) -> bool {
    matches!(
        version,
        CadVersion::Acad2004
            | CadVersion::Acad2007
            | CadVersion::Acad2010
            | CadVersion::Acad2013
            | CadVersion::Acad2018
    )
}

fn is_r2007_plus(version: CadVersion) -> bool {
    matches!(
        version,
        CadVersion::Acad2007 | CadVersion::Acad2010 | CadVersion::Acad2013 | CadVersion::Acad2018
    )
}

fn object_type_name(object_type: u32, classes: &[DwgClassSummary]) -> Option<String> {
    fixed_object_type_name(object_type)
        .map(str::to_string)
        .or_else(|| {
            classes
                .iter()
                .find(|class| u32::from(class.class_number) == object_type)
                .map(|class| {
                    if !class.dxf_name.is_empty() {
                        class.dxf_name.clone()
                    } else if !class.cpp_class_name.is_empty() {
                        class.cpp_class_name.clone()
                    } else {
                        format!("CLASS#{}", class.class_number)
                    }
                })
        })
}

pub(crate) fn fixed_object_type_name(object_type: u32) -> Option<&'static str> {
    match object_type {
        0x001 => Some("TEXT"),
        0x002 => Some("ATTRIB"),
        0x003 => Some("ATTDEF"),
        0x004 => Some("BLOCK"),
        0x005 => Some("ENDBLK"),
        0x006 => Some("SEQEND"),
        0x007 => Some("INSERT"),
        0x008 => Some("MINSERT"),
        0x00A => Some("VERTEX_2D"),
        0x00B => Some("VERTEX_3D"),
        0x00C => Some("VERTEX_MESH"),
        0x00D => Some("VERTEX_PFACE"),
        0x00E => Some("VERTEX_PFACE_FACE"),
        0x00F => Some("POLYLINE_2D"),
        0x010 => Some("POLYLINE_3D"),
        0x011 => Some("ARC"),
        0x012 => Some("CIRCLE"),
        0x013 => Some("LINE"),
        0x014 => Some("DIMENSION_ORDINATE"),
        0x015 => Some("DIMENSION_LINEAR"),
        0x016 => Some("DIMENSION_ALIGNED"),
        0x017 => Some("DIMENSION_ANG_3_Pt"),
        0x018 => Some("DIMENSION_ANG_2_Ln"),
        0x019 => Some("DIMENSION_RADIUS"),
        0x01A => Some("DIMENSION_DIAMETER"),
        0x01B => Some("POINT"),
        0x01C => Some("3DFACE"),
        0x01D => Some("POLYLINE_PFACE"),
        0x01E => Some("POLYLINE_MESH"),
        0x01F => Some("SOLID"),
        0x020 => Some("TRACE"),
        0x021 => Some("SHAPE"),
        0x022 => Some("VIEWPORT"),
        0x023 => Some("ELLIPSE"),
        0x024 => Some("SPLINE"),
        0x025 => Some("REGION"),
        0x026 => Some("SOLID3D"),
        0x027 => Some("BODY"),
        0x028 => Some("RAY"),
        0x029 => Some("XLINE"),
        0x02A => Some("DICTIONARY"),
        0x02B => Some("OLEFRAME"),
        0x02C => Some("MTEXT"),
        0x02D => Some("LEADER"),
        0x02E => Some("TOLERANCE"),
        0x02F => Some("MLINE"),
        0x030 => Some("BLOCK_CONTROL_OBJ"),
        0x031 => Some("BLOCK_HEADER"),
        0x032 => Some("LAYER_CONTROL_OBJ"),
        0x033 => Some("LAYER"),
        0x034 => Some("STYLE_CONTROL_OBJ"),
        0x035 => Some("STYLE"),
        0x038 => Some("LTYPE_CONTROL_OBJ"),
        0x039 => Some("LTYPE"),
        0x03C => Some("VIEW_CONTROL_OBJ"),
        0x03D => Some("VIEW"),
        0x03E => Some("UCS_CONTROL_OBJ"),
        0x03F => Some("UCS"),
        0x040 => Some("VPORT_CONTROL_OBJ"),
        0x041 => Some("VPORT"),
        0x042 => Some("APPID_CONTROL_OBJ"),
        0x043 => Some("APPID"),
        0x044 => Some("DIMSTYLE_CONTROL_OBJ"),
        0x045 => Some("DIMSTYLE"),
        0x046 => Some("VP_ENT_HDR_CTRL_OBJ"),
        0x047 => Some("VP_ENT_HDR"),
        0x048 => Some("GROUP"),
        0x049 => Some("MLINESTYLE"),
        0x04A => Some("OLE2FRAME"),
        0x04B => Some("DUMMY"),
        0x04C => Some("LONG_TRANSACTION"),
        0x04D => Some("LWPOLYLINE"),
        0x04E => Some("HATCH"),
        0x04F => Some("XRECORD"),
        0x050 => Some("ACDBPLACEHOLDER"),
        0x051 => Some("VBA_PROJECT"),
        0x052 => Some("LAYOUT"),
        0x1F2 => Some("ACAD_PROXY_ENTITY"),
        0x1F3 => Some("ACAD_PROXY_OBJECT"),
        _ => None,
    }
}

fn parse_object_header(bytes: &[u8], bit_index: usize) -> Option<ParsedObjectHeader> {
    // R2010+ object record: MS(size) + MC(handle_stream_bits) + Type(BS) + Handle(H) + EED + body
    // Note: R2010+ does NOT have the RL(object_data_bits) field that R13-R2007 had.
    let mut reader = MsbBitReader::new(bytes);
    reader.bit_index = bit_index;
    let size_bytes = usize::try_from(read_modular_short_msb(&mut reader)?).ok()?;
    if size_bytes == 0 {
        return None;
    }
    let data_start_bits = reader.bit_index;
    let handle_stream_bits = read_modular_char_msb(&mut reader)?;
    let object_type = read_object_type_msb(&mut reader)?;
    // Self-handle immediately follows the type (no RL field in R2010+)
    let (declared_handle_code, declared_handle) = read_handle_reference_msb(&mut reader)?;
    // Skip EED blocks: repeated BS(size) + handle + data until size==0
    loop {
        let eed_size = read_bitshort(&mut reader).unwrap_or(0);
        if eed_size == 0 || eed_size > 4096 {
            break;
        }
        let _ = read_handle_reference_msb(&mut reader);
        for _ in 0..eed_size {
            reader.read_u8()?;
        }
    }
    // Skip graphic_present flag and optional graphics data
    let graphic_present = reader.read_bit().unwrap_or(false);
    if graphic_present {
        let graphics_size = read_bitlong_msb(&mut reader).unwrap_or(0) as usize;
        for _ in 0..graphics_size.min(65536) {
            let _ = reader.read_u8();
        }
    }
    Some(ParsedObjectHeader {
        size_bytes,
        data_start_bits,
        handle_stream_bits,
        object_type,
        object_data_bits: 0,
        declared_handle_code,
        declared_handle,
        post_header_bit_index: reader.bit_index,
    })
}

fn find_modern_object_start_candidate(
    bytes: &[u8],
    expected_handle: u64,
    raw_offset_bits: u64,
    next_raw_offset_bits: u64,
    classes: &[DwgClassSummary],
) -> Option<ParsedModernObjectStart> {
    const SEARCH_BACK_BITS: i64 = 64;
    const SEARCH_FORWARD_BITS: i64 = 24;
    const HANDLE_SEARCH_BITS: usize = 192;

    let raw_offset_bits = i64::try_from(raw_offset_bits).ok()?;
    let next_raw_offset_bits = i64::try_from(next_raw_offset_bits).ok()?;
    let mut best = None::<(i64, ParsedModernObjectStart)>;

    for delta in -SEARCH_BACK_BITS..=SEARCH_FORWARD_BITS {
        let candidate_start = raw_offset_bits + delta;
        if candidate_start < 0 {
            continue;
        }
        let Some(candidate) =
            parse_modern_object_start(bytes, usize::try_from(candidate_start).ok()?)
        else {
            continue;
        };
        let candidate_end_bits = i64::try_from(candidate.object_stream_start_bits)
            .ok()?
            .saturating_add(i64::try_from(candidate.size_bytes).ok()?.saturating_mul(8));
        let handle_section_start =
            candidate_end_bits - i64::try_from(candidate.handle_stream_bits).ok()?;
        if handle_section_start < i64::try_from(candidate.object_stream_start_bits).ok()? {
            continue;
        }
        if candidate_end_bits <= i64::try_from(candidate.object_stream_start_bits).ok()? {
            continue;
        }
        if candidate_end_bits > next_raw_offset_bits + 64 {
            continue;
        }
        let type_name = object_type_name(candidate.object_type, classes);
        let declared_handle =
            read_declared_handle_after_object_type(bytes, candidate.object_stream_start_bits);
        let handle_match =
            search_for_object_handle(bytes, candidate.record_start_bits, expected_handle).filter(
                |result| {
                    usize::try_from(result.delta_bits)
                        .ok()
                        .is_some_and(|delta| delta <= HANDLE_SEARCH_BITS)
                },
            );
        let mut score = 0i64;
        if type_name.is_some() {
            score += 1000;
        }
        if candidate.object_type <= 0x1F3 {
            score += 100;
        }
        if declared_handle == Some(expected_handle) {
            score += 10_000;
        }
        if let Some(result) = &handle_match {
            score += 4_000;
            score -= i64::from(result.delta_bits / 8);
        }
        score += 50;
        if candidate_end_bits >= raw_offset_bits {
            score += 25;
        }
        score -= delta.abs();
        score -= i64::try_from(candidate.size_bytes).ok()?.min(4096) / 256;
        if best
            .as_ref()
            .is_none_or(|(best_score, _)| score > *best_score)
        {
            best = Some((score, candidate));
        }
    }

    best.map(|(_, candidate)| candidate)
}

fn parse_modern_object_start(bytes: &[u8], bit_index: usize) -> Option<ParsedModernObjectStart> {
    let mut preamble_reader = MsbBitReader::new(bytes);
    preamble_reader.bit_index = bit_index;
    let size_start = preamble_reader.bit_index;
    let size_bytes = usize::try_from(read_modular_short_msb(&mut preamble_reader)?).ok()?;
    if size_bytes == 0 {
        return None;
    }
    let size_field_bytes = preamble_reader
        .bit_index
        .saturating_sub(size_start)
        .div_ceil(8);
    let handle_stream_bits = read_modular_char_msb(&mut preamble_reader)?;
    let object_stream_start_bits = preamble_reader.bit_index;
    let object_type = read_object_type_at(bytes, object_stream_start_bits)?;
    Some(ParsedModernObjectStart {
        record_start_bits: bit_index,
        object_stream_start_bits,
        size_bytes,
        size_field_bytes,
        handle_stream_bits,
        object_type,
    })
}

fn read_declared_handle_after_object_type(bytes: &[u8], bit_index: usize) -> Option<u64> {
    let mut reader = MsbBitReader::new(bytes);
    reader.bit_index = bit_index;
    let _object_type = read_object_type_msb(&mut reader)?;
    read_handle_reference_msb(&mut reader).map(|(_, value)| value)
}

fn parse_object_prefix(bytes: &[u8], bit_index: usize) -> ParsedObjectPrefix {
    let mut reader = BitReader::new(bytes);
    reader.bit_index = bit_index;
    let modular_size_hint = read_modular_char_from_bits(&mut reader);
    let modular_size_bytes =
        modular_size_hint.map(|_| reader.bit_index.saturating_sub(bit_index).div_ceil(8));
    let marker_be_hint = reader
        .read_bits(16)
        .and_then(|value| u16::try_from(value).ok());
    let flag_hint = reader.read_u8();
    let control_hint = reader.read_u8();
    ParsedObjectPrefix {
        modular_size_hint,
        modular_size_bytes,
        marker_be_hint,
        flag_hint,
        control_hint,
    }
}

fn read_modular_char_from_bits(reader: &mut BitReader<'_>) -> Option<u64> {
    let mut shift = 0usize;
    let mut last = reader.read_u8()?;
    let mut value = u64::from(last & 0x7F);
    while (last & 0x80) != 0 {
        shift += 7;
        last = reader.read_u8()?;
        value |= u64::from(last & 0x7F).checked_shl(shift as u32)?;
    }
    Some(value)
}

fn read_modular_short_msb(reader: &mut MsbBitReader<'_>) -> Option<u64> {
    let mut shift = 15usize;
    let mut low = reader.read_u8()?;
    let mut high = reader.read_u8()?;
    let mut value = u64::from(low) | (u64::from(high & 0x7F) << 8);
    while (high & 0x80) != 0 {
        low = reader.read_u8()?;
        high = reader.read_u8()?;
        value |= u64::from(low).checked_shl(u32::try_from(shift).ok()?)?;
        shift += 8;
        value |= u64::from(high & 0x7F).checked_shl(u32::try_from(shift).ok()?)?;
        shift += 7;
    }
    Some(value)
}

fn read_modular_char_msb(reader: &mut MsbBitReader<'_>) -> Option<u64> {
    let mut shift = 0usize;
    let mut last = reader.read_u8()?;
    let mut value = u64::from(last & 0x7F);
    while (last & 0x80) != 0 {
        shift += 7;
        last = reader.read_u8()?;
        value |= u64::from(last & 0x7F).checked_shl(u32::try_from(shift).ok()?)?;
    }
    Some(value)
}

fn read_handle_reference(reader: &mut BitReader<'_>) -> Option<(u8, u64)> {
    let code = u8::try_from(reader.read_bits(4)?).ok()?;
    let byte_count = usize::try_from(reader.read_bits(4)?).ok()?;
    let mut value = 0u64;
    for _ in 0..byte_count {
        value = (value << 8) | u64::from(reader.read_u8()?);
    }
    Some((code, value))
}

fn read_handle_reference_msb(reader: &mut MsbBitReader<'_>) -> Option<(u8, u64)> {
    let code = u8::try_from(reader.read_bits(4)?).ok()?;
    let byte_count = usize::try_from(reader.read_bits(4)?).ok()?;
    let mut value = 0u64;
    for _ in 0..byte_count {
        value = (value << 8) | u64::from(reader.read_u8()?);
    }
    Some((code, value))
}

fn search_for_object_handle(
    bytes: &[u8],
    start_bit_index: usize,
    expected_handle: u64,
) -> Option<HandleMatchSearchResult> {
    const MAX_HANDLE_SEARCH_DELTA_BITS: usize = 512;
    let to_delta_bits = |delta: usize| u32::try_from(delta).ok();
    (0..=MAX_HANDLE_SEARCH_DELTA_BITS).find_map(|delta_bits| {
        let bit_index = start_bit_index.saturating_add(delta_bits);
        let generic = read_handle_reference_at(bytes, bit_index)
            .filter(|(code, value)| *code == 0 && *value == expected_handle)
            .and_then(|_| {
                Some(HandleMatchSearchResult {
                    delta_bits: to_delta_bits(delta_bits)?,
                    encoding: "generic-code0",
                })
            });
        generic.or_else(|| {
            read_raw_handle_at(bytes, bit_index)
                .filter(|value| *value == expected_handle)
                .and_then(|_| {
                    Some(HandleMatchSearchResult {
                        delta_bits: to_delta_bits(delta_bits)?,
                        encoding: "raw-length",
                    })
                })
        })
    })
}

fn read_handle_reference_at(bytes: &[u8], bit_index: usize) -> Option<(u8, u64)> {
    let mut reader = BitReader::new(bytes);
    reader.bit_index = bit_index;
    read_handle_reference(&mut reader)
}

fn read_object_type_at(bytes: &[u8], bit_index: usize) -> Option<u32> {
    let mut reader = MsbBitReader::new(bytes);
    reader.bit_index = bit_index;
    read_object_type_msb(&mut reader)
}

fn read_raw_handle_at(bytes: &[u8], bit_index: usize) -> Option<u64> {
    let mut reader = BitReader::new(bytes);
    reader.bit_index = bit_index;
    let byte_count = usize::from(reader.read_u8()?);
    let mut value = 0u64;
    for _ in 0..byte_count {
        value = (value << 8) | u64::from(reader.read_u8()?);
    }
    Some(value)
}

fn read_stream_u8(bytes: &[u8], bit_index: usize) -> Option<u8> {
    let mut reader = BitReader::new(bytes);
    reader.bit_index = bit_index;
    reader.read_u8()
}

fn bitstream_hex_preview(bytes: &[u8], bit_index: usize, len: usize) -> String {
    let mut reader = BitReader::new(bytes);
    reader.bit_index = bit_index;
    let mut output = Vec::new();
    for _ in 0..len {
        let Some(value) = reader.read_u8() else {
            break;
        };
        output.push(format!("{value:02X}"));
    }
    output.join(" ")
}

#[allow(dead_code)]
fn read_object_type(reader: &mut BitReader<'_>) -> Option<u32> {
    match reader.read_bits(2)? {
        0b00 => Some(u32::from(reader.read_u8()?)),
        0b01 => Some(0x1F0 + u32::from(reader.read_u8()?)),
        0b10 | 0b11 => Some(u32::from(reader.read_u16_le()?)),
        _ => None,
    }
}

fn read_object_type_msb(reader: &mut MsbBitReader<'_>) -> Option<u32> {
    match reader.read_bits(2)? {
        0b00 => Some(u32::from(reader.read_u8()?)),
        0b01 => Some(0x1F0 + u32::from(reader.read_u8()?)),
        0b10 | 0b11 => Some(u32::from(reader.read_u16_le()?)),
        _ => None,
    }
}

#[allow(dead_code)]
fn read_bitlong(reader: &mut BitReader<'_>) -> Option<u32> {
    match reader.read_bits(2)? {
        0b00 => reader.read_u32_le(),
        0b01 => Some(u32::from(reader.read_u8()?)),
        0b10 => Some(0),
        0b11 => None,
        _ => None,
    }
}

fn read_bitlong_msb(reader: &mut MsbBitReader<'_>) -> Option<u32> {
    match reader.read_bits(2)? {
        0b00 => reader.read_u32_le(),
        0b01 => Some(u32::from(reader.read_u8()?)),
        0b10 => Some(0),
        0b11 => None,
        _ => None,
    }
}

fn object_span_delta_counts(objects: &[DwgObjectRecordSummary]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for object in objects {
        if let Some(delta) = object.span_delta_hint {
            *counts.entry(delta.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

fn object_header_profile_counts(objects: &[DwgObjectRecordSummary]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for object in objects {
        *counts.entry(object.header_profile.clone()).or_insert(0) += 1;
    }
    counts
}

fn object_header_marker_counts(objects: &[DwgObjectRecordSummary]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for object in objects {
        if let Some(marker) = object.marker_be_hint {
            *counts.entry(format!("{marker:04X}")).or_insert(0) += 1;
        }
    }
    counts
}

fn object_header_signature_counts(objects: &[DwgObjectRecordSummary]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for object in objects {
        *counts.entry(object.header_signature.clone()).or_insert(0) += 1;
    }
    counts
}

fn short_record_signature_counts(records: &[DwgShortObjectStub]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for record in records {
        *counts.entry(record.header_signature.clone()).or_insert(0) += 1;
    }
    counts
}

fn short_payload_signature_counts(records: &[DwgShortObjectStub]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for record in records {
        *counts.entry(record.payload_signature.clone()).or_insert(0) += 1;
    }
    counts
}

fn decode_short_object_stubs(
    object_index: &[DwgObjectRecordSummary],
    objects: &[u8],
) -> Vec<DwgShortObjectStub> {
    object_index
        .iter()
        .filter_map(|record| decode_short_object_stub(record, objects))
        .collect()
}

fn decode_short_object_stub(
    record: &DwgObjectRecordSummary,
    objects: &[u8],
) -> Option<DwgShortObjectStub> {
    let base_header_len = match record.header_profile.as_str() {
        "delta5-short" => 5,
        "delta6-short" => 6,
        _ => return None,
    };
    let start = usize::try_from(record.offset).ok()?;
    let bytes = objects.get(start..start.saturating_add(record.span_bytes))?;
    let payload = bytes.get(base_header_len..)?;
    let payload_len = payload.len();
    let payload_matches_size_hint = record
        .modular_size_hint
        .and_then(|size_hint| usize::try_from(size_hint).ok())
        .is_some_and(|size_hint| size_hint == payload_len);
    let extra_header_byte = (base_header_len > 5)
        .then(|| bytes.get(5).copied())
        .flatten();
    Some(DwgShortObjectStub {
        handle: record.handle,
        header_signature: record.header_signature.clone(),
        payload_offset_in_record: base_header_len,
        payload_len,
        payload_matches_size_hint,
        extra_header_byte,
        payload_signature: payload_signature(payload),
        payload_prefix_hex: hex_preview(payload, 12),
    })
}

fn extract_text_fragments(bytes: &[u8]) -> Vec<String> {
    let mut fragments = extract_utf16le_strings(bytes);
    fragments.sort();
    fragments.dedup();
    fragments
}

fn scan_system_pages_from_bytes(bytes: &[u8]) -> Vec<DwgSystemPageInfo> {
    let mut pages = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= bytes.len() {
        let signature = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        if let Some(kind) = DwgSystemPageKind::from_signature(signature) {
            if let Some(page) = parse_system_page(bytes, offset, kind) {
                pages.push(page);
            }
        }
        offset += 1;
    }
    pages
}

fn extract_section_descriptors(bytes: &[u8]) -> Vec<DwgSectionDescriptorSummary> {
    let system_pages = scan_system_pages_from_bytes(bytes);
    let page_map_records = system_pages
        .iter()
        .find(|page| page.kind == DwgSystemPageKind::SectionPageMap)
        .map(|page| parse_section_page_records(&page.decoded))
        .unwrap_or_default();
    system_pages
        .iter()
        .find(|page| page.kind == DwgSystemPageKind::SectionMap)
        .map(|page| parse_section_descriptors(&page.decoded, &page_map_records))
        .unwrap_or_default()
}

impl DwgSystemPageKind {
    fn from_signature(signature: u32) -> Option<Self> {
        match signature {
            SECTION_PAGE_MAP_SIGNATURE => Some(Self::SectionMap),
            SYSTEM_PAGE_SIGNATURE => Some(Self::SectionPageMap),
            _ => None,
        }
    }
}

fn parse_system_page(
    bytes: &[u8],
    offset: usize,
    kind: DwgSystemPageKind,
) -> Option<DwgSystemPageInfo> {
    if offset + SYSTEM_PAGE_HEADER_LEN > bytes.len() {
        return None;
    }
    let compressed_size = u32::from_le_bytes(bytes[offset + 8..offset + 12].try_into().ok()?);
    let compression_type = u32::from_le_bytes(bytes[offset + 12..offset + 16].try_into().ok()?);
    let uncompressed_size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().ok()?);
    let payload_start = offset + SYSTEM_PAGE_HEADER_LEN;
    let payload_end = payload_start.checked_add(compressed_size as usize)?;
    if payload_end > bytes.len() {
        return None;
    }
    let payload = &bytes[payload_start..payload_end];
    let decode = decode_page_payload(payload, compression_type, uncompressed_size as usize);
    let mut strings = extract_ascii_strings(&decode.output);
    strings.truncate(MAX_PAGE_STRINGS);
    Some(DwgSystemPageInfo {
        offset: offset as u64,
        kind,
        decompressor: DwgSystemPageCompression::from_code(compression_type),
        compressed_size,
        uncompressed_size,
        decoded_size: decode.output.len(),
        complete: decode.complete,
        decoded: decode.output,
        strings,
        warning: decode.warning,
    })
}

impl DwgSystemPageCompression {
    fn from_code(code: u32) -> Self {
        match code {
            1 => Self::Uncompressed,
            2 => Self::Compressed,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Debug)]
struct DecodeResult {
    output: Vec<u8>,
    complete: bool,
    warning: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct DwgDataSectionHeader {
    page_type: u32,
    section_number: u32,
    compressed_size: u32,
    page_size: u32,
    start_offset: u32,
    checksum: u32,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct ParsedObjectHeader {
    size_bytes: usize,
    data_start_bits: usize,
    handle_stream_bits: u64,
    object_type: u32,
    object_data_bits: u32,
    declared_handle_code: u8,
    declared_handle: u64,
    post_header_bit_index: usize,
}

#[derive(Debug, Clone, Copy)]
struct ParsedObjectPrefix {
    modular_size_hint: Option<u64>,
    modular_size_bytes: Option<usize>,
    marker_be_hint: Option<u16>,
    flag_hint: Option<u8>,
    control_hint: Option<u8>,
}

#[derive(Debug, Clone, Copy)]
struct ParsedModernObjectStart {
    record_start_bits: usize,
    object_stream_start_bits: usize,
    size_bytes: usize,
    size_field_bytes: usize,
    handle_stream_bits: u64,
    object_type: u32,
}

#[derive(Debug, Clone, Copy)]
struct HandleMatchSearchResult {
    delta_bits: u32,
    encoding: &'static str,
}

#[allow(dead_code)]
struct BitReader<'a> {
    bytes: &'a [u8],
    bit_index: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_index: 0,
        }
    }

    fn read_bits(&mut self, count: usize) -> Option<u32> {
        let mut value = 0u32;
        for bit_offset in 0..count {
            let byte = *self.bytes.get(self.bit_index / 8)?;
            let shift = self.bit_index % 8;
            value |= u32::from((byte >> shift) & 1) << bit_offset;
            self.bit_index += 1;
        }
        Some(value)
    }

    fn read_u8(&mut self) -> Option<u8> {
        self.read_bits(8).and_then(|value| u8::try_from(value).ok())
    }

    fn read_u16_le(&mut self) -> Option<u16> {
        let low = self.read_u8()?;
        let high = self.read_u8()?;
        Some(u16::from(low) | (u16::from(high) << 8))
    }

    fn read_u32_le(&mut self) -> Option<u32> {
        let b0 = self.read_u8()?;
        let b1 = self.read_u8()?;
        let b2 = self.read_u8()?;
        let b3 = self.read_u8()?;
        Some(u32::from(b0) | (u32::from(b1) << 8) | (u32::from(b2) << 16) | (u32::from(b3) << 24))
    }
}

struct MsbBitReader<'a> {
    bytes: &'a [u8],
    bit_index: usize,
}

impl<'a> MsbBitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_index: 0,
        }
    }

    fn read_bit(&mut self) -> Option<bool> {
        let byte = *self.bytes.get(self.bit_index / 8)?;
        let shift = 7usize.saturating_sub(self.bit_index % 8);
        self.bit_index += 1;
        Some(((byte >> shift) & 1) != 0)
    }

    fn read_bits(&mut self, count: usize) -> Option<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            value = (value << 1) | u32::from(self.read_bit()?);
        }
        Some(value)
    }

    fn read_u8(&mut self) -> Option<u8> {
        self.read_bits(8).and_then(|value| u8::try_from(value).ok())
    }

    fn read_u16_le(&mut self) -> Option<u16> {
        let low = self.read_u8()?;
        let high = self.read_u8()?;
        Some(u16::from(low) | (u16::from(high) << 8))
    }

    fn read_u32_le(&mut self) -> Option<u32> {
        let b0 = self.read_u8()?;
        let b1 = self.read_u8()?;
        let b2 = self.read_u8()?;
        let b3 = self.read_u8()?;
        Some(u32::from(b0) | (u32::from(b1) << 8) | (u32::from(b2) << 16) | (u32::from(b3) << 24))
    }
}

fn decode_page_payload(
    payload: &[u8],
    compression_type: u32,
    expected_size: usize,
) -> DecodeResult {
    match DwgSystemPageCompression::from_code(compression_type) {
        DwgSystemPageCompression::Uncompressed => DecodeResult {
            output: payload[..payload.len().min(expected_size)].to_vec(),
            complete: payload.len() >= expected_size,
            warning: (payload.len() < expected_size).then(|| {
                format!(
                    "decoded {} of {} bytes from uncompressed page payload",
                    payload.len(),
                    expected_size
                )
            }),
        },
        DwgSystemPageCompression::Compressed | DwgSystemPageCompression::Unknown(_) => {
            decode_system_page(payload, expected_size)
        }
    }
}

fn decode_system_page(payload: &[u8], expected_size: usize) -> DecodeResult {
    let mut output = Vec::with_capacity(expected_size.min(4096));
    if payload.is_empty() {
        return DecodeResult {
            output,
            complete: expected_size == 0,
            warning: (expected_size != 0).then(|| "compressed system page is empty".to_string()),
        };
    }

    let mut source_index = 0usize;
    let mut opcode = match read_u8(payload, &mut source_index) {
        Ok(opcode) => opcode,
        Err(warning) => {
            return DecodeResult {
                output,
                complete: false,
                warning: Some(warning),
            }
        }
    };

    if (opcode & 0xF0) == 0 {
        let literal_len = match read_literal_count(opcode, payload, &mut source_index) {
            Ok(length) => length + 3,
            Err(warning) => {
                return DecodeResult {
                    output,
                    complete: false,
                    warning: Some(warning),
                }
            }
        };
        let remaining = expected_size.saturating_sub(output.len());
        opcode = match copy_literal_block(
            payload,
            &mut source_index,
            &mut output,
            literal_len,
            remaining,
        ) {
            Ok(opcode) => opcode,
            Err(warning) => {
                return DecodeResult {
                    output,
                    complete: false,
                    warning: Some(warning),
                }
            }
        };
    }

    while opcode != 0x11 && output.len() < expected_size {
        let mut copy_offset = 0usize;
        let copy_len = if !(0x10..0x40).contains(&opcode) {
            let copy_len = ((opcode >> 4) as usize).saturating_sub(1);
            let opcode2 = match read_u8(payload, &mut source_index) {
                Ok(opcode) => opcode,
                Err(warning) => {
                    return DecodeResult {
                        output,
                        complete: false,
                        warning: Some(warning),
                    }
                }
            };
            copy_offset = ((((opcode >> 2) & 0x03) as usize) | ((opcode2 as usize) << 2)) + 1;
            copy_len
        } else if opcode < 0x20 {
            let copy_len = match read_compressed_bytes(opcode, 0x07, payload, &mut source_index) {
                Ok(length) => length,
                Err(warning) => {
                    return DecodeResult {
                        output,
                        complete: false,
                        warning: Some(warning),
                    }
                }
            };
            copy_offset = ((opcode & 0x08) as usize) << 11;
            opcode = match two_byte_offset(&mut copy_offset, 0x4000, payload, &mut source_index) {
                Ok(opcode) => opcode,
                Err(warning) => {
                    return DecodeResult {
                        output,
                        complete: false,
                        warning: Some(warning),
                    }
                }
            };
            copy_len
        } else {
            let copy_len = match read_compressed_bytes(opcode, 0x1F, payload, &mut source_index) {
                Ok(length) => length,
                Err(warning) => {
                    return DecodeResult {
                        output,
                        complete: false,
                        warning: Some(warning),
                    }
                }
            };
            opcode = match two_byte_offset(&mut copy_offset, 1, payload, &mut source_index) {
                Ok(opcode) => opcode,
                Err(warning) => {
                    return DecodeResult {
                        output,
                        complete: false,
                        warning: Some(warning),
                    }
                }
            };
            copy_len
        };

        let remaining = expected_size.saturating_sub(output.len());
        copy_from_history(&mut output, copy_offset, copy_len.min(remaining));

        let mut literal_len = (opcode & 0x03) as usize;
        if literal_len == 0 {
            opcode = match read_u8(payload, &mut source_index) {
                Ok(opcode) => opcode,
                Err(warning) => {
                    return DecodeResult {
                        output,
                        complete: false,
                        warning: Some(warning),
                    }
                }
            };
            if (opcode & 0xF0) == 0 {
                literal_len = match read_literal_count(opcode, payload, &mut source_index) {
                    Ok(length) => length + 3,
                    Err(warning) => {
                        return DecodeResult {
                            output,
                            complete: false,
                            warning: Some(warning),
                        }
                    }
                };
            }
        }

        if literal_len > 0 {
            let remaining = expected_size.saturating_sub(output.len());
            opcode = match copy_literal_block(
                payload,
                &mut source_index,
                &mut output,
                literal_len,
                remaining,
            ) {
                Ok(opcode) => opcode,
                Err(warning) => {
                    return DecodeResult {
                        output,
                        complete: false,
                        warning: Some(warning),
                    }
                }
            };
        }
    }

    DecodeResult {
        complete: output.len() == expected_size,
        warning: (output.len() < expected_size).then(|| {
            format!(
                "decoded {} of {} bytes before payload exhaustion",
                output.len(),
                expected_size
            )
        }),
        output,
    }
}

fn copy_literal_block(
    payload: &[u8],
    index: &mut usize,
    output: &mut Vec<u8>,
    count: usize,
    max_write: usize,
) -> Result<u8, String> {
    let end = index.saturating_add(count);
    if end > payload.len() {
        return Err(format!(
            "literal overrun while decoding system page (need {count} bytes, have {})",
            payload.len().saturating_sub(*index)
        ));
    }
    let write_end = index.saturating_add(count.min(max_write));
    output.extend_from_slice(&payload[*index..write_end]);
    *index = end;
    if *index >= payload.len() {
        return Ok(0x11);
    }
    read_u8(payload, index)
}

fn read_literal_count(code: u8, payload: &[u8], index: &mut usize) -> Result<usize, String> {
    let mut count = (code & 0x0F) as usize;
    if count == 0 {
        let mut last = read_u8(payload, index)? as usize;
        while last == 0 {
            count += 0xFF;
            last = read_u8(payload, index)? as usize;
        }
        count += 0x0F + last;
    }
    Ok(count)
}

fn read_compressed_bytes(
    opcode: u8,
    valid_bits: u8,
    payload: &[u8],
    index: &mut usize,
) -> Result<usize, String> {
    let mut count = (opcode & valid_bits) as usize;
    if count == 0 {
        let mut last = read_u8(payload, index)? as usize;
        while last == 0 {
            count += 0xFF;
            last = read_u8(payload, index)? as usize;
        }
        count += valid_bits as usize + last;
    }
    Ok(count + 2)
}

fn two_byte_offset(
    offset: &mut usize,
    added_value: usize,
    payload: &[u8],
    index: &mut usize,
) -> Result<u8, String> {
    let first_byte = read_u8(payload, index)?;
    *offset |= (first_byte as usize) >> 2;
    *offset |= (read_u8(payload, index)? as usize) << 6;
    *offset += added_value;
    Ok(first_byte)
}

fn copy_from_history(output: &mut Vec<u8>, offset: usize, len: usize) {
    let mut source_index = output.len() as isize - offset as isize;
    for _ in 0..len {
        if source_index < 0 {
            output.push(0);
        } else {
            output.push(output[source_index as usize]);
        }
        source_index += 1;
    }
}

fn read_u8(payload: &[u8], index: &mut usize) -> Result<u8, String> {
    payload
        .get(*index)
        .copied()
        .ok_or_else(|| "unexpected end of compressed system page".to_string())
        .inspect(|_| *index += 1)
}

fn read_i32_cursor(bytes: &[u8], index: &mut usize) -> Option<i32> {
    let value = i32::from_le_bytes(bytes.get(*index..(*index + 4))?.try_into().ok()?);
    *index += 4;
    Some(value)
}

fn read_u32_cursor(bytes: &[u8], index: &mut usize) -> Option<u32> {
    let value = u32::from_le_bytes(bytes.get(*index..(*index + 4))?.try_into().ok()?);
    *index += 4;
    Some(value)
}

fn read_u64_cursor(bytes: &[u8], index: &mut usize) -> Option<u64> {
    let value = u64::from_le_bytes(bytes.get(*index..(*index + 8))?.try_into().ok()?);
    *index += 8;
    Some(value)
}

fn u32_from_slice(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn u64_from_slice(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

fn read_fixed_string(bytes: &[u8], index: &mut usize, len: usize) -> Option<String> {
    let slice = bytes.get(*index..(*index + len))?;
    *index += len;
    let end = slice
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(slice.len());
    Some(String::from_utf8_lossy(&slice[..end]).trim().to_string())
}

fn read_modular_char(bytes: &[u8], index: &mut usize) -> Result<u64, DwgReadError> {
    let mut shift = 0usize;
    let mut last = read_cursor_u8(bytes, index)?;
    let mut value = (last & 0x7F) as u64;
    while (last & 0x80) != 0 {
        shift += 7;
        last = read_cursor_u8(bytes, index)?;
        value |= ((last & 0x7F) as u64)
            .checked_shl(shift as u32)
            .ok_or_else(|| {
                DwgReadError::MalformedSection(
                    "modular char offset overflowed while parsing Handles".to_string(),
                )
            })?;
    }
    Ok(value)
}

fn read_signed_modular_char(bytes: &[u8], index: &mut usize) -> Result<i64, DwgReadError> {
    let mut last = read_cursor_u8(bytes, index)?;
    if (last & 0x80) == 0 {
        let value = (last & 0x3F) as i64;
        return Ok(if (last & 0x40) != 0 { -value } else { value });
    }

    let mut total_shift = 0usize;
    let mut sum = (last & 0x7F) as i64;
    loop {
        total_shift += 7;
        last = read_cursor_u8(bytes, index)?;
        if (last & 0x80) != 0 {
            sum |= ((last & 0x7F) as i64)
                .checked_shl(total_shift as u32)
                .ok_or_else(|| {
                    DwgReadError::MalformedSection(
                        "signed modular char overflowed while parsing Handles".to_string(),
                    )
                })?;
        } else {
            let value = sum
                | (((last & 0x3F) as i64)
                    .checked_shl(total_shift as u32)
                    .ok_or_else(|| {
                        DwgReadError::MalformedSection(
                            "signed modular char tail overflowed while parsing Handles".to_string(),
                        )
                    })?);
            return Ok(if (last & 0x40) != 0 { -value } else { value });
        }
    }
}

fn read_cursor_u8(bytes: &[u8], index: &mut usize) -> Result<u8, DwgReadError> {
    let value = *bytes.get(*index).ok_or_else(|| {
        DwgReadError::MalformedSection("unexpected end of reconstructed section buffer".to_string())
    })?;
    *index += 1;
    Ok(value)
}

fn parse_section_page_records(decoded: &[u8]) -> Vec<DwgSectionPageRecord> {
    let mut records = Vec::new();
    let mut index = 0usize;
    let mut seeker = 0x100_u64;
    while index + 8 <= decoded.len() {
        let Some(number) = read_i32_cursor(decoded, &mut index) else {
            break;
        };
        let Some(size) = read_u32_cursor(decoded, &mut index) else {
            break;
        };
        let is_gap = number < 0;
        records.push(DwgSectionPageRecord {
            number,
            size,
            seeker,
            is_gap,
        });
        if is_gap {
            if index + 16 > decoded.len() {
                break;
            }
            index += 16;
        }
        seeker = seeker.saturating_add(size as u64);
    }
    records
}

fn parse_section_descriptors(
    decoded: &[u8],
    page_records: &[DwgSectionPageRecord],
) -> Vec<DwgSectionDescriptorSummary> {
    if decoded.len() < 20 {
        return Vec::new();
    }
    let mut index = 0usize;
    let Some(descriptor_count) = read_u32_cursor(decoded, &mut index) else {
        return Vec::new();
    };
    index = index.saturating_add(16);

    let mut sections = Vec::new();
    for _ in 0..descriptor_count {
        if index + 96 > decoded.len() {
            break;
        }
        let Some(section_size) = read_u64_cursor(decoded, &mut index) else {
            break;
        };
        let Some(page_count) = read_u32_cursor(decoded, &mut index) else {
            break;
        };
        let Some(max_page_size) = read_u32_cursor(decoded, &mut index) else {
            break;
        };
        let _unknown = read_u32_cursor(decoded, &mut index);
        let Some(compressed_code) = read_u32_cursor(decoded, &mut index) else {
            break;
        };
        let Some(section_id) = read_u32_cursor(decoded, &mut index) else {
            break;
        };
        let Some(encrypted) = read_u32_cursor(decoded, &mut index) else {
            break;
        };
        let Some(raw_name) = read_fixed_string(decoded, &mut index, 64) else {
            break;
        };
        let name = normalize_section_name(&raw_name).unwrap_or(raw_name);
        let mut pages = Vec::new();
        for page_index in 0..page_count {
            if index + 16 > decoded.len() {
                break;
            }
            let Some(page_number) = read_u32_cursor(decoded, &mut index) else {
                break;
            };
            let Some(compressed_size) = read_u32_cursor(decoded, &mut index) else {
                break;
            };
            let Some(offset) = read_u64_cursor(decoded, &mut index) else {
                break;
            };
            let is_last_page = page_index + 1 == page_count;
            let mut decompressed_size = max_page_size;
            let remainder = section_size % max_page_size.max(1) as u64;
            if is_last_page && remainder > 0 {
                decompressed_size = remainder as u32;
            }
            let seeker = page_records
                .iter()
                .find(|record| record.number == page_number as i32)
                .map(|record| record.seeker);
            pages.push(DwgLocalSectionPageSummary {
                page_number,
                compressed_size,
                offset,
                decompressed_size,
                seeker,
            });
        }
        if name.is_empty() {
            continue;
        }
        sections.push(DwgSectionDescriptorSummary {
            section_id,
            name,
            section_size,
            page_count,
            max_page_size,
            compressed_code,
            encrypted,
            pages,
        });
    }
    sections
}

fn extract_ascii_strings(bytes: &[u8]) -> Vec<String> {
    let mut strings = Vec::new();
    let mut current = String::new();
    for byte in bytes {
        if is_printable_ascii(*byte) {
            current.push(*byte as char);
        } else if current.len() >= MIN_TEXT_FRAGMENT_LEN {
            strings.push(current.trim().to_string());
            current.clear();
        } else {
            current.clear();
        }
    }
    if current.len() >= MIN_TEXT_FRAGMENT_LEN {
        strings.push(current.trim().to_string());
    }
    strings.sort();
    strings.dedup();
    strings
}

fn infer_section_names(
    pages: &[DwgSystemPageInfo],
    sections: &[DwgSectionDescriptorSummary],
) -> Vec<String> {
    let mut names = sections
        .iter()
        .map(|section| section.name.clone())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    names.extend(
        pages
            .iter()
            .flat_map(|page| page.strings.iter())
            .filter_map(|string| normalize_section_name(string)),
    );
    names.sort();
    names.dedup();
    names.truncate(MAX_SECTION_NAMES);
    names
}

fn normalize_section_name(value: &str) -> Option<String> {
    let trimmed = value
        .trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != ':');
    if let Some(canonical) = canonical_section_name(trimmed) {
        return Some(canonical.to_string());
    }
    if trimmed.starts_with("AcDb:") {
        return Some(trimmed.to_string());
    }
    if matches!(
        trimmed,
        "AppInfo"
            | "SummaryInfo"
            | "RevHistory"
            | "Objects"
            | "Template"
            | "Handles"
            | "Classes"
            | "AuxHeader"
    ) {
        return Some(trimmed.to_string());
    }
    None
}

fn canonical_section_name(value: &str) -> Option<&'static str> {
    let upper = value.to_ascii_uppercase();
    if upper.starts_with("ACDB:FILEDEPLIS") {
        return Some("AcDb:FileDepList");
    }
    if upper.starts_with("ACDB:APPINFO") {
        return Some("AppInfo");
    }
    if upper.starts_with("ACDB:PREVIEW") {
        return Some("Preview");
    }
    if upper.starts_with("ACDB:REVHISTORY") {
        return Some("RevHistory");
    }
    if upper.starts_with("ACDB:ACDBOBJECTS") || upper.starts_with("ACDB:OBJECTS") {
        return Some("Objects");
    }
    if upper.starts_with("ACDB:HANDLES") {
        return Some("Handles");
    }
    if upper.starts_with("ACDB:CLASSES") {
        return Some("Classes");
    }
    if upper.starts_with("ACDB:HEADER") {
        return Some("AcDb:Header");
    }
    if upper.ends_with("OBJFREESPACEP") || upper.contains("OBJFREESPACE") {
        return Some("AcDb:ObjFreeSpace");
    }
    if upper.contains("SUMMARY") || upper.ends_with("MARY") {
        return Some("SummaryInfo");
    }
    if upper.starts_with("APPINFO") {
        return Some("AppInfo");
    }
    if upper.starts_with("REVHISTORY") {
        return Some("RevHistory");
    }
    if upper.starts_with("OBJECTS") {
        return Some("Objects");
    }
    if upper.starts_with("TEMPLAT") {
        return Some("Template");
    }
    if upper.starts_with("HANDLE") {
        return Some("Handles");
    }
    if upper.starts_with("CLASS") {
        return Some("Classes");
    }
    if upper.ends_with("UXHEADER") || upper.contains("AUXHEADER") {
        return Some("AuxHeader");
    }
    None
}

fn infer_candidate_layers(fragments: &[String]) -> Vec<String> {
    let mut candidates = fragments
        .iter()
        .filter(|fragment| looks_like_layer_name(fragment))
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    candidates
}

fn looks_like_layer_name(fragment: &str) -> bool {
    if fragment.len() < MIN_TEXT_FRAGMENT_LEN || fragment.len() > 64 {
        return false;
    }
    if fragment.contains('\\') || fragment.contains('/') || fragment.contains(".shx") {
        return false;
    }
    let uppercase = fragment
        .chars()
        .filter(|character| character.is_ascii_uppercase())
        .count();
    let alphabetic = fragment
        .chars()
        .filter(|character| character.is_ascii_alphabetic())
        .count();
    let separator = fragment
        .chars()
        .filter(|character| matches!(character, '_' | '-'))
        .count();
    let digit = fragment
        .chars()
        .filter(|character| character.is_ascii_digit())
        .count();
    let whitespace = fragment
        .chars()
        .filter(|character| character.is_ascii_whitespace())
        .count();
    if alphabetic < 2 || whitespace > 2 {
        return false;
    }
    uppercase * 2 >= alphabetic || separator > 0 || digit > 0
}

fn object_header_signature(
    header_profile: &str,
    marker_be_hint: Option<u16>,
    flag_hint: Option<u8>,
    control_hint: Option<u8>,
) -> String {
    let marker = marker_be_hint
        .map(|marker| format!("{marker:04X}"))
        .unwrap_or_else(|| "????".to_string());
    let flag = flag_hint
        .map(|flag| format!("{flag:02X}"))
        .unwrap_or_else(|| "??".to_string());
    let control = control_hint
        .map(|control| format!("{control:02X}"))
        .unwrap_or_else(|| "??".to_string());
    format!("{header_profile}:{marker}:{flag}:{control}")
}

fn object_header_profile(
    modular_size_hint: Option<u64>,
    modular_size_bytes: Option<usize>,
    span_bytes: usize,
) -> String {
    let Some(size_hint) = modular_size_hint else {
        return "unknown".to_string();
    };
    let Some(span_bytes) = i64::try_from(span_bytes).ok() else {
        return "unknown".to_string();
    };
    let delta = span_bytes - size_hint as i64;
    match (modular_size_bytes, delta) {
        (Some(1), 5) => "delta5-short".to_string(),
        (Some(1), 6) => "delta6-short".to_string(),
        (Some(2), 133) => "delta133-wide".to_string(),
        (Some(width), other) => format!("delta{other}-w{width}"),
        (None, other) => format!("delta{other}-unknown-width"),
    }
}

fn hex_preview(bytes: &[u8], len: usize) -> String {
    bytes
        .iter()
        .take(len)
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn payload_signature(bytes: &[u8]) -> String {
    hex_preview(bytes, 4)
}

fn keyword_hint_counts(fragments: &[String]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for keyword in STRUCTURAL_KEYWORDS {
        let count = fragments
            .iter()
            .filter(|fragment| fragment.to_ascii_uppercase().contains(keyword))
            .count();
        if count > 0 {
            counts.insert((*keyword).to_string(), count);
        }
    }
    counts
}

fn extract_utf16le_strings(bytes: &[u8]) -> Vec<String> {
    let mut fragments = Vec::new();
    let mut index = 0;
    while index + 1 < bytes.len() {
        let mut end = index;
        while end + 1 < bytes.len() && is_printable_ascii(bytes[end]) && bytes[end + 1] == 0 {
            end += 2;
        }
        if end > index {
            let fragment = bytes[index..end]
                .chunks_exact(2)
                .map(|chunk| chunk[0] as char)
                .collect::<String>();
            if fragment.len() >= MIN_TEXT_FRAGMENT_LEN && is_meaningful_text(&fragment) {
                fragments.push(fragment);
            }
            index = end;
        } else {
            index += 1;
        }
    }
    fragments
}

fn is_printable_ascii(byte: u8) -> bool {
    matches!(byte, 32..=126)
}

fn is_meaningful_text(fragment: &str) -> bool {
    let mut allowed = 0;
    let mut alphabetic = 0;
    for character in fragment.chars() {
        if character.is_ascii_alphabetic() {
            alphabetic += 1;
        }
        if character.is_ascii_alphanumeric()
            || matches!(
                character,
                ' ' | '_' | '-' | ':' | '/' | '\\' | '.' | '(' | ')' | '{' | '}' | '[' | ']' | '"'
            )
        {
            allowed += 1;
        }
    }
    let total = fragment.chars().count();
    alphabetic >= 2 && allowed * 5 >= total * 4
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Write;

    use tempfile::NamedTempFile;

    #[test]
    fn probe_file_reads_known_dwg_sentinel() {
        let mut file = NamedTempFile::new().expect("temp file should be created");
        file.write_all(b"AC1032rest-of-file")
            .expect("temp dwg should be written");

        let probe = probe_file(file.path()).expect("probe should succeed");
        assert_eq!(probe.version, CadVersion::Acad2018);
        assert_eq!(probe.sentinel, "AC1032");
    }

    #[test]
    fn probe_file_rejects_unknown_signature() {
        let mut file = NamedTempFile::new().expect("temp file should be created");
        file.write_all(b"ZZ9999rest-of-file")
            .expect("temp dwg should be written");

        let error = probe_file(file.path()).expect_err("probe should fail");
        assert!(matches!(error, DwgReadError::UnsupportedSignature(_)));
    }

    #[test]
    fn read_stub_document_uses_probe_version() {
        let mut file = NamedTempFile::new().expect("temp file should be created");
        file.write_all(b"AC1015rest-of-file")
            .expect("temp dwg should be written");

        let document = read_stub_document(file.path()).expect("stub document should load");
        assert_eq!(document.format, CadFormat::Dwg);
        assert_eq!(document.version, CadVersion::Acad2000);
        assert!(document.entities.is_empty());
    }

    #[test]
    fn probe_extracts_utf16_fragments() {
        let mut file = NamedTempFile::new().expect("temp file should be created");
        file.write_all(b"AC1032")
            .expect("temp dwg sentinel should be written");
        file.write_all(&[0, 0, 0, 0])
            .expect("padding should be written");
        file.write_all(&[b'H', 0, b'O', 0, b'J', 0, b'D', 0])
            .expect("utf16 fragment should be written");

        let probe = probe_file(file.path()).expect("probe should succeed");
        assert!(probe
            .text_fragments
            .iter()
            .any(|fragment| fragment == "HOJD"));
    }

    #[test]
    fn summarize_file_infers_candidate_layers_and_keywords() {
        let mut file = NamedTempFile::new().expect("temp file should be created");
        file.write_all(b"AC1032")
            .expect("temp dwg sentinel should be written");
        file.write_all(&[0, 0, 0, 0])
            .expect("padding should be written");
        write_utf16_fragment(&mut file, "HOJDKURVA_1_2");
        write_utf16_fragment(&mut file, "LAYER");
        write_utf16_fragment(&mut file, "Some path C:\\Data\\font.shx");

        let summary = summarize_file(file.path()).expect("summary should succeed");
        assert!(summary.file_header.is_none());
        assert!(summary
            .candidate_layers
            .iter()
            .any(|fragment| fragment == "HOJDKURVA_1_2"));
        assert_eq!(summary.keyword_hint_counts.get("LAYER"), Some(&1));
        assert!(!summary
            .candidate_layers
            .iter()
            .any(|fragment| fragment.contains(".shx")));
    }

    #[test]
    fn write_summary_json_persists_structural_summary() {
        let mut file = NamedTempFile::new().expect("temp file should be created");
        file.write_all(b"AC1032")
            .expect("temp dwg sentinel should be written");
        write_utf16_fragment(&mut file, "NYBYG");

        let output = NamedTempFile::new().expect("output file should be created");
        let summary = write_summary_json(file.path(), output.path()).expect("summary should write");
        let persisted = std::fs::read_to_string(output.path()).expect("json should be readable");
        assert!(persisted.contains("NYBYG"));
        assert!(persisted.contains("Acad2018"));
        assert_eq!(summary.probe.sentinel, "AC1032");
    }

    #[test]
    fn summarize_local_sample_reports_known_fragments_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };
        let summary = summarize_file(&path).expect("local sample summary should succeed");
        assert_eq!(summary.probe.version, CadVersion::Acad2018);
        assert_eq!(
            summary
                .file_header
                .as_ref()
                .map(|header| header.file_id.as_str()),
            Some("AcFssFcAJMB")
        );
        assert!(summary
            .candidate_layers
            .iter()
            .any(|fragment| fragment == "HOJDKURVA_1_2"));
        assert!(summary
            .summary_fragments
            .iter()
            .any(|fragment| fragment == "NYBYG"));
        assert!(summary
            .section_names
            .iter()
            .any(|name| name == "AppInfo" || name == "RevHistory" || name == "Objects"));
        assert!(
            !summary.system_pages.is_empty(),
            "real sample should expose native system pages"
        );
        assert!(
            summary
                .page_map_records
                .iter()
                .any(|record| record.number == 7),
            "real sample should expose section page records"
        );
        assert!(
            summary
                .sections
                .iter()
                .any(|section| section.name == "Objects"),
            "real sample should expose structured section descriptors"
        );
        assert_eq!(
            summary.class_count, 8,
            "real sample should expose native class definitions"
        );
        assert!(
            summary
                .class_sample
                .iter()
                .any(|class| class.dxf_name == "VISUALSTYLE"),
            "real sample should decode known class names from the Classes section"
        );
        assert!(
            summary.handle_count > 0,
            "real sample should expose native handle offsets"
        );
        assert!(
            !summary.object_span_delta_counts.is_empty(),
            "real sample should expose object span diagnostics"
        );
        assert!(
            summary.short_record_count > 0,
            "real sample should expose short native object records"
        );
        assert!(
            summary.short_record_payload_match_count > 0,
            "real sample should confirm at least some short-record payload boundaries"
        );
        assert!(
            !summary.object_index_sample.is_empty(),
            "real sample should expose a native object index sample"
        );
        assert!(
            !summary.short_record_sample.is_empty(),
            "real sample should expose decoded short-record samples"
        );
    }

    #[test]
    fn read_local_sample_sections_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };
        let header = read_section_data(&path, "AcDb:Header").expect("header section should decode");
        let classes = read_classes(&path).expect("classes should decode");
        let handle_map = read_handle_map(&path).expect("handle map should decode");
        let object_index = read_object_index(&path).expect("object index should decode");
        let handles = read_section_data(&path, "Handles").expect("handles section should decode");
        let objects = read_section_data(&path, "Objects").expect("objects section should decode");
        let mut object_type_counts = std::collections::BTreeMap::<String, usize>::new();
        let unknown_type_count = object_index
            .iter()
            .filter(|record| record.object_type_name.is_none())
            .count();
        let mut untyped_header_counts = std::collections::BTreeMap::<String, usize>::new();
        let mut untyped_masked_type_counts = std::collections::BTreeMap::<String, usize>::new();
        let mut untyped_samples = Vec::new();
        let lwpolyline_handles = object_index
            .iter()
            .filter_map(|record| {
                match record.object_type_name.as_deref() {
                    Some(type_name) => {
                        *object_type_counts.entry(type_name.to_string()).or_default() += 1;
                        (type_name == "LWPOLYLINE").then_some(record.handle)
                    }
                    None => {
                        *untyped_header_counts
                            .entry(record.header_signature.clone())
                            .or_default() += 1;
                        if let Some(masked_name) = record
                            .object_type
                            .and_then(|object_type| fixed_object_type_name(object_type & 0x01FF))
                        {
                            *untyped_masked_type_counts
                                .entry(masked_name.to_string())
                                .or_default() += 1;
                        }
                        if untyped_samples.len() < 16 {
                            untyped_samples.push(format!(
                                "{:X}: profile={} prefix={} handle_match={:?} declared={:?} object_type={:?}",
                                record.handle,
                                record.header_profile,
                                record.prefix_hex,
                                record.handle_match_search_delta_bits,
                                record.declared_handle,
                                record.object_type,
                            ));
                        }
                        None
                    }
                }
            })
            .take(16)
            .collect::<Vec<_>>();
        let block_header_records = object_index
            .iter()
            .filter(|record| record.object_type_name.as_deref() == Some("BLOCK_HEADER"))
            .map(|record| {
                format!(
                    "{:X}: type={:?} name={:?} profile={} prefix={} handle_match={:?} declared={:?}",
                    record.handle,
                    record.object_type,
                    record.object_type_name,
                    record.header_profile,
                    record.prefix_hex,
                    record.handle_match_search_delta_bits,
                    record.declared_handle,
                )
            })
            .collect::<Vec<_>>();
        let masked_block_header_records = object_index
            .iter()
            .filter(|record| {
                record.object_type_name.is_none()
                    && record
                        .object_type
                        .and_then(|object_type| fixed_object_type_name(object_type & 0x01FF))
                        == Some("BLOCK_HEADER")
            })
            .map(|record| {
                format!(
                    "{:X}: type={:?} masked=BLOCK_HEADER profile={} prefix={} handle_match={:?} declared={:?}",
                    record.handle,
                    record.object_type,
                    record.header_profile,
                    record.prefix_hex,
                    record.handle_match_search_delta_bits,
                    record.declared_handle,
                )
            })
            .collect::<Vec<_>>();
        println!(
            "sample object index size: total={} typed={} untyped={}",
            object_index.len(),
            object_index.len().saturating_sub(unknown_type_count),
            unknown_type_count,
        );
        println!("sample object type counts: {object_type_counts:?}");
        println!("sample typed block header records: {block_header_records:?}");
        println!("sample masked block header records: {masked_block_header_records:?}");
        println!("sample lwpolyline handles: {lwpolyline_handles:?}");
        println!("sample untyped header counts: {untyped_header_counts:?}");
        println!("sample untyped masked-type counts: {untyped_masked_type_counts:?}");
        println!("sample untyped samples: {untyped_samples:?}");
        let oracle_lwpolyline_handles = [0x346_u64, 0x347, 0x35F, 0x87, 0x88, 0x89, 0x8A];
        let oracle_handle_offsets = oracle_lwpolyline_handles
            .iter()
            .map(|handle| format!("{handle:X}:{:?}", handle_map.get(handle)))
            .collect::<Vec<_>>();
        println!("oracle handle map offsets: {oracle_handle_offsets:?}");
        let oracle_records = object_index
            .iter()
            .filter(|record| oracle_lwpolyline_handles.contains(&record.handle))
            .map(|record| {
                format!(
                    "{:X}: type={:?} name={:?} profile={} prefix={} handle_match={:?} declared={:?}",
                    record.handle,
                    record.object_type,
                    record.object_type_name,
                    record.header_profile,
                    record.prefix_hex,
                    record.handle_match_search_delta_bits,
                    record.declared_handle,
                )
            })
            .collect::<Vec<_>>();
        println!("oracle lwpolyline native records: {oracle_records:?}");
        if let Some((&oracle_handle, &raw_offset_bytes)) =
            handle_map.iter().find(|(handle, _)| **handle == 0x346)
        {
            let raw_offset_bits = usize::try_from(raw_offset_bytes).unwrap_or_default() * 8;
            let raw_offset_bits_direct = usize::try_from(raw_offset_bytes).unwrap_or_default();
            let mut candidates = Vec::new();
            for delta in -64isize..=24 {
                let candidate_start = raw_offset_bits as isize + delta;
                if candidate_start < 0 {
                    continue;
                }
                let candidate_start =
                    usize::try_from(candidate_start).expect("candidate start should fit");
                let Some(candidate) = parse_modern_object_start(&objects, candidate_start) else {
                    continue;
                };
                candidates.push(format!(
                    "handle {:X} delta {} start {} object_start {} type {:?} name {:?} size {} handle_bits {}",
                    oracle_handle,
                    delta,
                    candidate_start,
                    candidate.object_stream_start_bits,
                    candidate.object_type,
                    object_type_name(candidate.object_type, &classes),
                    candidate.size_bytes,
                    candidate.handle_stream_bits,
                ));
            }
            println!("oracle handle 346 candidates: {candidates:?}");
            let mut direct_candidates = Vec::new();
            for delta in -64isize..=24 {
                let candidate_start = raw_offset_bits_direct as isize + delta;
                if candidate_start < 0 {
                    continue;
                }
                let candidate_start =
                    usize::try_from(candidate_start).expect("candidate start should fit");
                let Some(candidate) = parse_modern_object_start(&objects, candidate_start) else {
                    continue;
                };
                direct_candidates.push(format!(
                    "handle {:X} raw-bit delta {} start {} object_start {} type {:?} name {:?} size {} handle_bits {}",
                    oracle_handle,
                    delta,
                    candidate_start,
                    candidate.object_stream_start_bits,
                    candidate.object_type,
                    object_type_name(candidate.object_type, &classes),
                    candidate.size_bytes,
                    candidate.handle_stream_bits,
                ));
            }
            println!("oracle handle 346 direct-bit candidates: {direct_candidates:?}");
        }
        assert_eq!(header.len(), 1512);
        assert_eq!(classes.len(), 8);
        assert_eq!(handles.len(), 4609);
        assert_eq!(objects.len(), 150_387);
        assert!(classes.iter().any(|class| class.dxf_name == "VISUALSTYLE"));
        assert!(!handle_map.is_empty());
        assert!(!object_index.is_empty());
        assert!(
            object_index
                .iter()
                .any(|record| record.modular_size_hint.is_some()),
            "native object records should expose leading modular-size hints"
        );
        assert!(
            object_index
                .iter()
                .any(|record| {
                    matches!(
                        record.object_type_name.as_deref(),
                        Some("LAYER")
                            | Some("BLOCK_HEADER")
                            | Some("INSERT")
                            | Some("ARC")
                            | Some("LWPOLYLINE")
                            | Some("POLYLINE_2D")
                            | Some("POLYLINE_3D")
                    )
                }),
            "native object records should surface fixed DWG object type names when decoding succeeds"
        );
        let short_records = decode_short_object_stubs(&object_index, &objects);
        assert!(!short_records.is_empty());
        assert!(
            short_records
                .iter()
                .any(|record| record.payload_matches_size_hint),
            "short native object stubs should confirm payload framing"
        );
        assert!(
            object_index
                .windows(2)
                .all(|window| window[0].offset_bits <= window[1].offset_bits),
            "object index should be sorted by native object bit offset"
        );
        let matched_handle = object_index
            .iter()
            .find(|record| record.handle == 624)
            .expect("sample should expose a known insert handle");
        assert!(matched_handle.offset_bits > 0);
        assert_eq!(matched_handle.object_type_name.as_deref(), Some("INSERT"));
        assert_ne!(
            matched_handle.prefix_hex,
            "00 00 00 00 00 00 00 00 00 00 00 00"
        );
        assert!(header.iter().any(|byte| *byte != 0));
        assert!(objects.iter().any(|byte| *byte != 0));
    }

    #[test]
    fn read_local_sample_document_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };
        let document = read_document(&path).expect("sample document should decode");
        let semantic_probes = [
            0x270_u64, 0x2A, 0x6F, 0x22D, 0x233, 0x30, 0x31, 0x32, 0x33, 0x47, 0x8B,
        ]
        .into_iter()
        .filter_map(|handle| {
            probe_semantic_record(&path, handle)
                .ok()
                .flatten()
                .map(|probe| format!("{handle:X}: {:?}", probe))
        })
        .collect::<Vec<_>>();
        let polyline_count = document
            .entities
            .iter()
            .filter(|entity| matches!(entity, cadio_ir::Entity::Polyline(_)))
            .count();
        let line_count = document
            .entities
            .iter()
            .filter(|entity| matches!(entity, cadio_ir::Entity::Line(_)))
            .count();
        let insert_count = document
            .entities
            .iter()
            .filter(|entity| matches!(entity, cadio_ir::Entity::Insert(_)))
            .count();
        println!(
            "sample native document: top-level={} blocks={} polylines={} lines={} inserts={}",
            document.entities.len(),
            document.blocks.len(),
            polyline_count,
            line_count,
            insert_count,
        );
        println!(
            "sample native layers: count={} names={:?}",
            document.layers.len(),
            document
                .layers
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>()
        );
        println!(
            "sample native top-level inserts: {:?}",
            document
                .entities
                .iter()
                .filter_map(|entity| match entity {
                    cadio_ir::Entity::Insert(insert) => Some((
                        insert.common.handle.as_deref().unwrap_or_default(),
                        insert.block_name.as_str(),
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>()
        );
        println!(
            "sample native block names: {:?}",
            document
                .blocks
                .iter()
                .map(|block| block.name.as_str())
                .collect::<Vec<_>>()
        );
        println!("sample semantic probes around known inserts: {semantic_probes:?}");
        for block in &document.blocks {
            let block_polyline_count = block
                .entities
                .iter()
                .filter(|entity| matches!(entity, cadio_ir::Entity::Polyline(_)))
                .count();
            let block_insert_count = block
                .entities
                .iter()
                .filter(|entity| matches!(entity, cadio_ir::Entity::Insert(_)))
                .count();
            println!(
                "block {}: entities={} polylines={} inserts={}",
                block.name,
                block.entities.len(),
                block_polyline_count,
                block_insert_count,
            );
            if block_insert_count > 0 {
                println!(
                    "block {} insert names: {:?}",
                    block.name,
                    block
                        .entities
                        .iter()
                        .filter_map(|entity| match entity {
                            cadio_ir::Entity::Insert(insert) => Some(insert.block_name.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                );
            }
        }
        assert_eq!(document.format, CadFormat::Dwg);
        assert_eq!(document.version, CadVersion::Acad2018);
        assert!(
            !document.entities.is_empty(),
            "sample document should recover at least some native top-level entities"
        );
        assert!(
            document
                .entities
                .iter()
                .any(|entity| matches!(entity, cadio_ir::Entity::Polyline(_))),
            "sample document should recover native polyline geometry"
        );
        assert!(
            document
                .blocks
                .iter()
                .any(|block| block.name == "berg_i_dagen"),
            "sample document should preserve the known survey block definition"
        );
    }

    #[test]
    fn probe_local_sample_insert_handles_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        for handle in [0x30_u64, 0x31, 0x32, 0x33, 0x34, 0x35, 0x6F, 0x270] {
            println!(
                "record {handle:X}: {:?}",
                probe_record(&path, handle).expect("probe record should succeed")
            );
            println!(
                "semantic {handle:X}: {:?}",
                probe_semantic_record(&path, handle).expect("probe semantic should succeed")
            );
        }

        let probe = probe_file(&path).expect("probe should succeed");
        let classes = read_classes(&path).expect("classes should decode");
        let handle_map = read_handle_map(&path).expect("handle map should decode");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");
        println!(
            "0x33 candidates: {:?}",
            decode::debug_object_start_candidates(
                probe.version,
                &handle_map,
                &object_index,
                &objects,
                &classes,
                0x33
            )
        );
        println!(
            "0x47 candidates: {:?}",
            decode::debug_object_start_candidates(
                probe.version,
                &handle_map,
                &object_index,
                &objects,
                &classes,
                0x47
            )
        );
        println!(
            "0x8B candidates: {:?}",
            decode::debug_object_start_candidates(
                probe.version,
                &handle_map,
                &object_index,
                &objects,
                &classes,
                0x8B
            )
        );
        let type_hints = decode::object_type_hints(&object_index);
        println!(
            "0x47 scored: {:?}",
            decode::debug_scored_candidates(
                probe.version,
                &handle_map,
                &object_index,
                &objects,
                &classes,
                &type_hints,
                0x47,
            )
        );
        println!(
            "0x8B scored: {:?}",
            decode::debug_scored_candidates(
                probe.version,
                &handle_map,
                &object_index,
                &objects,
                &classes,
                &type_hints,
                0x8B,
            )
        );
        println!(
            "0x270 scored: {:?}",
            decode::debug_scored_candidates(
                probe.version,
                &handle_map,
                &object_index,
                &objects,
                &classes,
                &type_hints,
                0x270,
            )
        );
        let insert_type_hints = decode::object_type_hints(&object_index);
        let insert_270_offset = object_index
            .iter()
            .find(|record| record.handle == 0x270)
            .and_then(|record| usize::try_from(record.raw_offset_bits).ok())
            .expect("sample insert should exist");
        println!(
            "0x270 nearby insert: {:?}",
            decode::debug_forced_insert_nearby_starts(
                probe.version,
                &objects,
                insert_270_offset,
                0x270,
                false,
                &insert_type_hints,
            )
        );
        println!(
            "0x270 exact insert: {:?}",
            decode::debug_forced_insert_exact_starts(
                probe.version,
                &handle_map,
                &object_index,
                &objects,
                0x270,
                false,
                &insert_type_hints,
            )
        );
        let minsert_33_offset = object_index
            .iter()
            .find(|record| record.handle == 0x33)
            .and_then(|record| usize::try_from(record.raw_offset_bits).ok())
            .expect("sample minsert should exist");
        println!(
            "0x33 nearby minsert: {:?}",
            decode::debug_forced_insert_nearby_starts(
                probe.version,
                &objects,
                minsert_33_offset,
                0x33,
                true,
                &insert_type_hints,
            )
        );
        println!(
            "0x33 exact minsert: {:?}",
            decode::debug_forced_insert_exact_starts(
                probe.version,
                &handle_map,
                &object_index,
                &objects,
                0x33,
                true,
                &insert_type_hints,
            )
        );
    }

    #[test]
    fn probe_local_sample_vertex_handles_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let classes = read_classes(&path).expect("classes should decode");
        let handle_map = read_handle_map(&path).expect("handle map should decode");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");
        let type_hints = decode::object_type_hints(&object_index);

        for handle in [0x89_u64, 0xB7, 0x12C, 0x270, 0x2C3] {
            println!(
                "vertex-probe index {handle:X}: {:?}",
                object_index
                    .iter()
                    .find(|record| record.handle == handle)
                    .map(|record| (
                        record.object_type,
                        record.object_type_name.as_deref(),
                        record.object_data_bits,
                        record.handle_stream_bits,
                        record.header_profile.as_str(),
                        record.prefix_hex.as_str(),
                        record.handle_match_search_delta_bits,
                        record.declared_handle,
                    ))
            );
            println!(
                "vertex-probe record {handle:X}: {:?}",
                probe_record(&path, handle).expect("probe record should succeed")
            );
            println!(
                "vertex-probe semantic {handle:X}: {:?}",
                probe_semantic_record(&path, handle).expect("probe semantic should succeed")
            );
            println!(
                "vertex-probe candidates {handle:X}: {:?}",
                decode::debug_object_start_candidates(
                    probe.version,
                    &handle_map,
                    &object_index,
                    &objects,
                    &classes,
                    handle,
                )
            );
            println!(
                "vertex-probe scored {handle:X}: {:?}",
                decode::debug_scored_candidates(
                    probe.version,
                    &handle_map,
                    &object_index,
                    &objects,
                    &classes,
                    &type_hints,
                    handle,
                )
            );
        }
    }

    #[test]
    fn probe_local_sample_polyline_neighborhood_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let classes = read_classes(&path).expect("classes should decode");
        let handle_map = read_handle_map(&path).expect("handle map should decode");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");
        let type_hints = decode::object_type_hints(&object_index);

        for handle in 0xB0_u64..=0xC5_u64 {
            if object_index.iter().all(|record| record.handle != handle) {
                continue;
            }
            println!(
                "poly-neighborhood index {handle:X}: {:?}",
                object_index
                    .iter()
                    .find(|record| record.handle == handle)
                    .map(|record| (
                        record.object_type,
                        record.object_type_name.as_deref(),
                        record.object_data_bits,
                        record.handle_stream_bits,
                        record.header_profile.as_str(),
                        record.prefix_hex.as_str(),
                        record.handle_match_search_delta_bits,
                        record.declared_handle,
                    ))
            );
            println!(
                "poly-neighborhood record {handle:X}: {:?}",
                probe_record(&path, handle).expect("probe record should succeed")
            );
            println!(
                "poly-neighborhood semantic {handle:X}: {:?}",
                probe_semantic_record(&path, handle).expect("probe semantic should succeed")
            );
            println!(
                "poly-neighborhood candidates {handle:X}: {:?}",
                decode::debug_object_start_candidates(
                    probe.version,
                    &handle_map,
                    &object_index,
                    &objects,
                    &classes,
                    handle,
                )
            );
            println!(
                "poly-neighborhood scored {handle:X}: {:?}",
                decode::debug_scored_candidates(
                    probe.version,
                    &handle_map,
                    &object_index,
                    &objects,
                    &classes,
                    &type_hints,
                    handle,
                )
            );
        }
    }

    #[test]
    fn probe_local_sample_polyline_window_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let classes = read_classes(&path).expect("classes should decode");
        let handle_map = read_handle_map(&path).expect("handle map should decode");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");
        let type_hints = decode::object_type_hints(&object_index);

        for handle in 0xB5_u64..=0xBC_u64 {
            if object_index.iter().all(|record| record.handle != handle) {
                continue;
            }
            println!(
                "poly-window index {handle:X}: {:?}",
                object_index
                    .iter()
                    .find(|record| record.handle == handle)
                    .map(|record| (
                        record.object_type,
                        record.object_type_name.as_deref(),
                        record.object_data_bits,
                        record.handle_stream_bits,
                        record.header_profile.as_str(),
                        record.prefix_hex.as_str(),
                        record.handle_match_search_delta_bits,
                        record.declared_handle,
                    ))
            );
            println!(
                "poly-window record {handle:X}: {:?}",
                probe_record(&path, handle).expect("probe record should succeed")
            );
            println!(
                "poly-window semantic {handle:X}: {:?}",
                probe_semantic_record(&path, handle).expect("probe semantic should succeed")
            );
            println!(
                "poly-window candidates {handle:X}: {:?}",
                decode::debug_object_start_candidates(
                    probe.version,
                    &handle_map,
                    &object_index,
                    &objects,
                    &classes,
                    handle,
                )
            );
            println!(
                "poly-window scored {handle:X}: {:?}",
                decode::debug_scored_candidates(
                    probe.version,
                    &handle_map,
                    &object_index,
                    &objects,
                    &classes,
                    &type_hints,
                    handle,
                )
            );
        }
    }

    #[test]
    fn probe_local_sample_forced_owned_vertex_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let handle_map = read_handle_map(&path).expect("handle map should decode");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");

        for handle in [0xB8_u64, 0x12C] {
            println!(
                "forced-vertex {handle:X}: {:?}",
                decode::debug_forced_vertex_probe(
                    probe.version,
                    &handle_map,
                    &object_index,
                    &objects,
                    handle,
                )
            );
            let raw_offset_bits = object_index
                .iter()
                .find(|record| record.handle == handle)
                .and_then(|record| usize::try_from(record.offset_bits).ok())
                .expect("sample handle should exist in object index");
            println!(
                "forced-vertex nearby {handle:X}: {:?}",
                decode::debug_forced_vertex_nearby_starts(
                    probe.version,
                    &objects,
                    raw_offset_bits,
                    handle,
                )
            );
        }
    }

    #[test]
    fn probe_local_sample_exact_offset_vertex_candidates_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let handle_map = read_handle_map(&path).expect("handle map should decode");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");

        let handles = decode::debug_object_start_candidates(
            probe.version,
            &handle_map,
            &object_index,
            &objects,
            &read_classes(&path).expect("classes should decode"),
            0x12C,
        );
        println!("exact-start probe handles 12C: {handles:?}");

        let sorted = object_index
            .iter()
            .filter_map(|record| {
                usize::try_from(record.offset_bits)
                    .ok()
                    .map(|offset_bits| (record.handle, offset_bits))
            })
            .collect::<Vec<_>>();
        let preferred_offset_bits = handle_map
            .get(&0x12C)
            .and_then(|raw_offset_bytes| usize::try_from(*raw_offset_bytes).ok())
            .and_then(|offset_bytes| offset_bytes.checked_mul(8))
            .expect("sample handle should exist in handle map");
        let alternate_offset_bits = object_index
            .iter()
            .find(|record| record.handle == 0x12C)
            .and_then(|record| usize::try_from(record.offset_bits).ok())
            .filter(|offset_bits| *offset_bits != preferred_offset_bits);
        let next_offset_bits = sorted
            .iter()
            .position(|(handle, _)| *handle == 0x12C)
            .and_then(|index| sorted.get(index + 1).map(|(_, offset_bits)| *offset_bits));
        println!(
            "exact-offset vertex candidates 12C: {:?}",
            decode::debug_exact_offset_vertex_candidates(
                probe.version,
                &objects,
                0x12C,
                preferred_offset_bits,
                alternate_offset_bits,
                next_offset_bits,
            )
        );
    }

    #[test]
    fn probe_local_sample_forced_lwpolyline_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");

        for handle in [0x87_u64, 0x88, 0x89, 0x8A] {
            let raw_offset_bits = object_index
                .iter()
                .find(|record| record.handle == handle)
                .and_then(|record| usize::try_from(record.offset_bits).ok())
                .expect("sample handle should exist in object index");
            println!(
                "forced-lwpolyline nearby {handle:X}: {:?}",
                decode::debug_forced_lwpolyline_nearby_starts(
                    probe.version,
                    &objects,
                    raw_offset_bits,
                    handle,
                )
            );
        }
    }

    #[test]
    fn probe_local_sample_polyline3d_layouts_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let handle_map = read_handle_map(&path).expect("handle map should decode");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");

        for handle in [0xA3_u64, 0xB7, 0xC7, 0xE8] {
            println!(
                "polyline3d layouts {handle:X}: {:?}",
                decode::debug_polyline3d_layouts_for_handle(
                    probe.version,
                    &handle_map,
                    &object_index,
                    &objects,
                    handle,
                )
            );
        }
    }

    #[test]
    fn probe_local_sample_polyline3d_handle_sequences_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let handle_map = read_handle_map(&path).expect("handle map should decode");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");

        for handle in [0xA3_u64, 0xB7, 0xC7] {
            println!(
                "polyline3d handles {handle:X}: {:?}",
                decode::debug_polyline3d_handle_sequence_for_handle(
                    probe.version,
                    &handle_map,
                    &object_index,
                    &objects,
                    handle,
                )
            );
        }
    }

    #[test]
    fn probe_local_sample_polyline3d_nearby_starts_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");

        for handle in [0xB7_u64, 0xC7, 0xE8] {
            let raw_offset_bits = object_index
                .iter()
                .find(|record| record.handle == handle)
                .and_then(|record| usize::try_from(record.raw_offset_bits).ok())
                .expect("sample handle should exist in object index");
            println!(
                "polyline3d nearby {handle:X}: {:?}",
                decode::debug_forced_polyline3d_nearby_starts(
                    probe.version,
                    &objects,
                    raw_offset_bits,
                    handle,
                )
            );
        }
    }

    #[test]
    fn probe_local_sample_object_type_counts_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let object_index = read_object_index(&path).expect("object index should decode");
        let mut counts = std::collections::BTreeMap::<String, usize>::new();
        for record in object_index {
            let key = record
                .object_type_name
                .unwrap_or_else(|| "<unknown>".to_string());
            *counts.entry(key).or_insert(0) += 1;
        }
        println!("object-type counts: {counts:?}");
    }

    #[test]
    fn probe_local_sample_unknown_lwpolyline_candidates_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let object_index = read_object_index(&path).expect("object index should decode");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");

        let mut shown = 0usize;
        for record in object_index
            .iter()
            .filter(|record| record.object_type_name.is_none())
            .take(16)
        {
            let Some(raw_offset_bits) = usize::try_from(record.raw_offset_bits).ok() else {
                continue;
            };
            let candidates = decode::debug_forced_lwpolyline_nearby_starts(
                probe.version,
                &objects,
                raw_offset_bits,
                record.handle,
            );
            if candidates.is_empty() {
                continue;
            }
            shown += 1;
            println!(
                "unknown lwpolyline candidates {:X}: {candidates:?}",
                record.handle
            );
            if shown >= 8 {
                break;
            }
        }
    }

    #[test]
    fn probe_local_sample_table_handles_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let object_index = read_object_index(&path).expect("object index should decode");

        for kind in ["LAYER", "BLOCK_HEADER"] {
            let handles = object_index
                .iter()
                .filter_map(|record| {
                    (record.object_type_name.as_deref() == Some(kind)).then_some(record.handle)
                })
                .collect::<Vec<_>>();
            println!("{kind} handles: {handles:X?}");
            for handle in handles {
                println!(
                    "{kind} semantic {handle:X}: {:?}",
                    probe_semantic_record(&path, handle).expect("semantic probe should succeed")
                );
            }
        }
    }

    #[test]
    fn probe_local_sample_direct_vertex_body_offsets_when_available() {
        let Some(path) = local_sample_dwg() else {
            return;
        };

        let probe = probe_file(&path).expect("probe should succeed");
        let objects = read_section_data(&path, "Objects").expect("objects should decode");

        println!(
            "direct-vertex body 12C: {:?}",
            decode::debug_direct_vertex_body_offsets(
                probe.version,
                &objects,
                0x12C,
                810320..=810340,
            )
        );
        println!(
            "direct-vertex body 12C msb: {:?}",
            decode::debug_direct_vertex_body_offsets_msb(probe.version, &objects, 810320..=810340,)
        );
    }

    fn write_utf16_fragment(file: &mut NamedTempFile, text: &str) {
        for byte in text.encode_utf16().flat_map(|unit| unit.to_le_bytes()) {
            file.write_all(&[byte])
                .expect("utf16 byte should be written");
        }
        file.write_all(&[0, 0])
            .expect("terminator should be written");
    }

    fn local_sample_dwg() -> Option<PathBuf> {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let samples_dir = repo_root.join("tmp/dwg-samples");
        let mut entries = std::fs::read_dir(samples_dir).ok()?;
        entries
            .find_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("dwg"))
            })
    }
}
