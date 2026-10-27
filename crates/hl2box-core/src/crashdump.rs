//! Windows minidump analysis: where the process died, and why.
//!
//! Reads a `.mdmp` / `.dmp` written by the HL2SB crash handler (or a WER
//! minidump) and produces a [`Report`]:
//!
//! * the exception record and the **exception-time** register state (the
//!   thread list keeps a different, later context, so the exception stream is
//!   the one used here);
//! * the module list, each module's CodeView identity, and whether a matching
//!   PDB sits next to it;
//! * the faulting instruction, decoded, with its memory operand resolved
//!   against the registers and against RIP-relative globals in the dump;
//! * a stack scan from RSP where a value only counts as a frame when the
//!   bytes in front of it are a `call` and that code is present in the dump.
//!
//! Symbols come from the PDB next to the binary: `<engine>/hl2sb/bin/*.pdb`
//! and `<engine>/bin/*.pdb`. A PDB is used only when its GUID matches the
//! module's CodeView record, so a stale PDB never names the wrong function.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use iced_x86::{Decoder, DecoderOptions, Formatter, Instruction, IntelFormatter, OpKind, Register};
use msvc_demangler::DemangleFlags;

const MDMP_SIGNATURE: u32 = 0x504D_444D;
const STREAM_MODULE_LIST: u32 = 4;
const STREAM_MEMORY_LIST: u32 = 5;
const STREAM_EXCEPTION: u32 = 6;
const STREAM_MEMORY64_LIST: u32 = 9;

/// Refuse to load absurdly large dumps into memory.
pub const MAX_DUMP_BYTES: u64 = 1 << 30;
/// How many stack slots above RSP are scanned.
const STACK_SLOTS: usize = 512;
/// Instructions shown around the faulting one.
const ASM_BEFORE: usize = 6;
const ASM_AFTER: usize = 3;

#[derive(Debug, Clone)]
struct MemRange {
    start: u64,
    size: u64,
    file_off: usize,
}

/// A parsed dump: raw bytes plus the indexes needed to read memory by address.
pub struct Dump {
    data: Vec<u8>,
    streams: HashMap<u32, (usize, usize)>,
    ranges: Vec<MemRange>,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub name: String,
    pub path: String,
    pub base: u64,
    pub size: u64,
    /// CodeView RSDS identity: 16-byte GUID in on-disk order and the age.
    pub codeview: Option<([u8; 16], u32)>,
}

#[derive(Debug, Clone, Default)]
pub struct Registers {
    pub values: Vec<(&'static str, u64)>,
}

impl Registers {
    pub fn get(&self, name: &str) -> Option<u64> {
        self.values.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
    }
}

#[derive(Debug, Clone)]
pub struct Exception {
    pub thread_id: u32,
    pub code: u32,
    pub address: u64,
    pub params: Vec<u64>,
    pub registers: Option<Registers>,
}

/// Where an address lands: module, offset and (when a PDB matched) the
/// nearest public symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub module: String,
    pub offset: u64,
    pub symbol: Option<(String, u64)>,
}

impl Location {
    pub fn describe(&self) -> String {
        match &self.symbol {
            Some((name, off)) if *off == 0 => format!("{}!{}", self.module, name),
            Some((name, off)) => format!("{}!{}+0x{:X}", self.module, name, off),
            None => format!("{}+0x{:X}", self.module, self.offset),
        }
    }
}

/// What the launcher could do about a module's PDB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdbState {
    /// Found and identical to the module; this many named functions.
    Loaded(usize),
    /// No PDB next to the module.
    Missing,
    /// A PDB exists but was built from a different binary.
    Mismatch,
    /// A PDB exists but could not be read.
    Unreadable(String),
    /// The dump has no CodeView record for this module.
    NoIdentity,
    /// Not needed by this report, so the PDB was not opened at all.
    NotLoaded,
}

#[derive(Debug, Clone)]
pub struct ModuleRow {
    pub module: Module,
    pub pdb: PdbState,
}

#[derive(Debug, Clone)]
pub struct AsmRow {
    pub address: u64,
    pub text: String,
    pub fault: bool,
    pub note: Option<String>,
    /// Memory the instruction touches, when it can be computed: the global
    /// for RIP-relative operands, the dereferenced address for register bases.
    pub target: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub slot: usize,
    pub sp: u64,
    pub value: u64,
    pub location: Location,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub dump_path: String,
    pub size: u64,
    pub modules: Vec<ModuleRow>,
    pub exception: Option<ExceptionRow>,
    pub fault_asm: Vec<AsmRow>,
    pub frames: Vec<Frame>,
    /// Stack slots that look like code but whose call site is not in the dump.
    pub unverified_slots: usize,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExceptionRow {
    pub thread_id: u32,
    pub code: u32,
    pub code_name: &'static str,
    pub address: u64,
    pub location: Option<Location>,
    pub params: Vec<u64>,
    pub registers: Registers,
}

pub fn exception_code_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "EXCEPTION_ACCESS_VIOLATION",
        0xC000_00FD => "EXCEPTION_STACK_OVERFLOW",
        0xC000_0094 => "EXCEPTION_INT_DIVIDE_BY_ZERO",
        0xC000_001D => "EXCEPTION_ILLEGAL_INSTRUCTION",
        0xC000_0409 => "STATUS_STACK_BUFFER_OVERRUN",
        0xC000_0374 => "STATUS_HEAP_CORRUPTION",
        0x8000_0003 => "EXCEPTION_BREAKPOINT",
        0x4000_0015 => "STATUS_FATAL_APP_EXIT",
        0xE06D_7363 => "C++ exception (MSVC)",
        _ => "unknown",
    }
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

impl Dump {
    /// Parse a dump file. Fails on bad signatures or files over [`MAX_DUMP_BYTES`].
    pub fn open(path: &Path) -> Result<Dump, String> {
        let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
        if meta.len() > MAX_DUMP_BYTES {
            return Err(format!("dump is {} bytes, over the 1 GiB limit", meta.len()));
        }
        let data = std::fs::read(path).map_err(|e| e.to_string())?;
        Dump::from_bytes(data)
    }

    pub fn from_bytes(data: Vec<u8>) -> Result<Dump, String> {
        if u32_at(&data, 0) != Some(MDMP_SIGNATURE) {
            return Err("not a minidump (bad signature)".into());
        }
        let count = u32_at(&data, 8).ok_or("truncated header")? as usize;
        let dir = u32_at(&data, 12).ok_or("truncated header")? as usize;
        let mut streams = HashMap::new();
        for i in 0..count {
            let e = dir + i * 12;
            let stype = u32_at(&data, e).ok_or("truncated directory")?;
            let size = u32_at(&data, e + 4).ok_or("truncated directory")? as usize;
            let rva = u32_at(&data, e + 8).ok_or("truncated directory")? as usize;
            streams.entry(stype).or_insert((rva, size));
        }
        let mut dump = Dump { data, streams, ranges: Vec::new() };
        dump.index_memory()?;
        Ok(dump)
    }

    fn stream(&self, t: u32) -> Option<&[u8]> {
        let &(rva, size) = self.streams.get(&t)?;
        self.data.get(rva..rva + size)
    }

    fn index_memory(&mut self) -> Result<(), String> {
        let mut ranges = Vec::new();
        if let Some(s) = self.stream(STREAM_MEMORY64_LIST) {
            let n = u64_at(s, 0).unwrap_or(0) as usize;
            let mut off = u64_at(s, 8).unwrap_or(0) as usize;
            for i in 0..n {
                let e = 16 + i * 16;
                let (Some(start), Some(size)) = (u64_at(s, e), u64_at(s, e + 8)) else { break };
                ranges.push(MemRange { start, size, file_off: off });
                off += size as usize;
            }
        } else if let Some(s) = self.stream(STREAM_MEMORY_LIST) {
            let n = u32_at(s, 0).unwrap_or(0) as usize;
            for i in 0..n {
                let e = 4 + i * 16;
                let (Some(start), Some(size), Some(rva)) = (u64_at(s, e), u32_at(s, e + 8), u32_at(s, e + 12)) else {
                    break;
                };
                ranges.push(MemRange { start, size: size as u64, file_off: rva as usize });
            }
        }
        self.ranges = ranges;
        Ok(())
    }

    /// Bytes at a virtual address, when the whole span was captured.
    pub fn read(&self, va: u64, len: usize) -> Option<&[u8]> {
        for r in &self.ranges {
            if va >= r.start && va.checked_add(len as u64)? <= r.start + r.size {
                let off = r.file_off + (va - r.start) as usize;
                return self.data.get(off..off + len);
            }
        }
        None
    }

    pub fn read_u64(&self, va: u64) -> Option<u64> {
        u64_at(self.read(va, 8)?, 0)
    }

    pub fn modules(&self) -> Vec<Module> {
        let Some(s) = self.stream(STREAM_MODULE_LIST) else { return Vec::new() };
        let n = u32_at(s, 0).unwrap_or(0) as usize;
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let e = 4 + i * 108;
            let (Some(base), Some(size), Some(name_rva)) = (u64_at(s, e), u32_at(s, e + 8), u32_at(s, e + 20)) else {
                break;
            };
            let path = self.minidump_string(name_rva as usize).unwrap_or_default();
            let name = path.rsplit(['\\', '/']).next().unwrap_or("").to_string();
            let cv_size = u32_at(s, e + 76).unwrap_or(0) as usize;
            let cv_rva = u32_at(s, e + 80).unwrap_or(0) as usize;
            let codeview = self.data.get(cv_rva..cv_rva + cv_size).and_then(parse_rsds);
            out.push(Module { name, path, base, size: size as u64, codeview });
        }
        out
    }

    fn minidump_string(&self, rva: usize) -> Option<String> {
        let len = u32_at(&self.data, rva)? as usize;
        let raw = self.data.get(rva + 4..rva + 4 + len)?;
        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        Some(String::from_utf16_lossy(&units))
    }

    /// The exception stream, if the dump has one. The exception-time context
    /// is read from its own location, not from the thread list.
    pub fn exception(&self) -> Option<Exception> {
        let s = self.stream(STREAM_EXCEPTION)?;
        let thread_id = u32_at(s, 0)?;
        let code = u32_at(s, 8)?;
        let address = u64_at(s, 24)?;
        let nparams = (u32_at(s, 32)? as usize).min(15);
        let params = (0..nparams).filter_map(|i| u64_at(s, 40 + i * 8)).collect();
        let ctx_size = u32_at(s, 160)? as usize;
        let ctx_rva = u32_at(s, 164)? as usize;
        let registers = self.data.get(ctx_rva..ctx_rva + ctx_size).and_then(parse_context);
        Some(Exception { thread_id, code, address, params, registers })
    }

    pub fn is_mapped(&self, va: u64) -> bool {
        self.ranges.iter().any(|r| va >= r.start && va < r.start + r.size)
    }
}

fn parse_rsds(b: &[u8]) -> Option<([u8; 16], u32)> {
    if b.get(0..4)? != b"RSDS" {
        return None;
    }
    let mut guid = [0u8; 16];
    guid.copy_from_slice(b.get(4..20)?);
    Some((guid, u32_at(b, 20)?))
}

fn parse_context(c: &[u8]) -> Option<Registers> {
    const NAMES: [(&str, usize); 17] = [
        ("RAX", 0x78), ("RCX", 0x80), ("RDX", 0x88), ("RBX", 0x90),
        ("RSP", 0x98), ("RBP", 0xA0), ("RSI", 0xA8), ("RDI", 0xB0),
        ("R8", 0xB8), ("R9", 0xC0), ("R10", 0xC8), ("R11", 0xD0),
        ("R12", 0xD8), ("R13", 0xE0), ("R14", 0xE8), ("R15", 0xF0),
        ("RIP", 0xF8),
    ];
    let mut values = Vec::with_capacity(18);
    for (name, off) in NAMES {
        values.push((name, u64_at(c, off)?));
    }
    values.push(("EFLAGS", u32_at(c, 0x44)? as u64));
    Some(Registers { values })
}

/// Map a PDB whose GUID matches a module. Returns the sorted function table
/// (`rva`, demangled name) or why it was rejected.
pub fn load_pdb(path: &Path, guid: &[u8; 16]) -> Result<Vec<(u32, String)>, PdbState> {
    let file = std::fs::File::open(path).map_err(|e| PdbState::Unreadable(e.to_string()))?;
    let mut pdb = pdb::PDB::open(file).map_err(|e| PdbState::Unreadable(e.to_string()))?;
    let info = pdb.pdb_information().map_err(|e| PdbState::Unreadable(e.to_string()))?;
    if info.guid.to_bytes_le() != *guid {
        return Err(PdbState::Mismatch);
    }
    let address_map = pdb.address_map().map_err(|e| PdbState::Unreadable(e.to_string()))?;
    let table = pdb.global_symbols().map_err(|e| PdbState::Unreadable(e.to_string()))?;
    let mut funcs = Vec::new();
    let mut iter = table.iter();
    use pdb::FallibleIterator;
    while let Ok(Some(sym)) = iter.next() {
        if let Ok(pdb::SymbolData::Public(p)) = sym.parse() {
            if !(p.code || p.function) {
                continue;
            }
            let Some(rva) = p.offset.to_rva(&address_map) else { continue };
            let raw = p.name.to_string().into_owned();
            let name = msvc_demangler::demangle(&raw, DemangleFlags::llvm()).unwrap_or(raw);
            funcs.push((rva.0, name));
        }
    }
    funcs.sort_by_key(|(rva, _)| *rva);
    funcs.dedup_by_key(|(rva, _)| *rva);
    Ok(funcs)
}

/// Candidate PDB locations for a module: next to it in the engine tree
/// (`hl2sb/bin`, `bin`) and at the path the dump recorded, re-rooted under
/// the local engine folder.
pub fn pdb_candidates(engine_root: &Path, module: &Module) -> Vec<PathBuf> {
    let stem = module.name.rsplit_once('.').map(|(s, _)| s).unwrap_or(&module.name).to_string();
    let file = format!("{stem}.pdb");
    let mut out = vec![
        engine_root.join("hl2sb").join("bin").join(&file),
        engine_root.join("bin").join(&file),
    ];
    // "D:\srceng\hl2sb\bin\server.dll" -> <engine_root>\hl2sb\bin\server.pdb
    let parts: Vec<&str> = module.path.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    if let Some(i) = parts.iter().position(|p| p.eq_ignore_ascii_case("srceng")) {
        let mut tail = engine_root.to_path_buf();
        for p in &parts[i + 1..parts.len() - 1] {
            tail.push(p);
        }
        out.insert(0, tail.join(&file));
    }
    out
}

/// Choose the PDB for a module among its candidates and load it.
pub fn resolve_pdb(engine_root: &Path, module: &Module) -> (PdbState, Vec<(u32, String)>) {
    let Some((guid, _)) = module.codeview else {
        return (PdbState::NoIdentity, Vec::new());
    };
    let mut saw_mismatch = false;
    let mut last_err = None;
    for candidate in pdb_candidates(engine_root, module) {
        if !candidate.is_file() {
            continue;
        }
        match load_pdb(&candidate, &guid) {
            Ok(funcs) => return (PdbState::Loaded(funcs.len()), funcs),
            Err(PdbState::Mismatch) => saw_mismatch = true,
            Err(other) => last_err = Some(other),
        }
    }
    if saw_mismatch {
        (PdbState::Mismatch, Vec::new())
    } else if let Some(e) = last_err {
        (e, Vec::new())
    } else {
        (PdbState::Missing, Vec::new())
    }
}

/// Resolve addresses against the module list and the loaded symbol tables.
struct Resolver {
    modules: Vec<Module>,
    symbols: Vec<Vec<(u32, String)>>,
}

impl Resolver {
    fn module_of(&self, va: u64) -> Option<usize> {
        self.modules.iter().position(|m| va >= m.base && va < m.base + m.size)
    }

    fn locate(&self, va: u64) -> Option<Location> {
        let i = self.module_of(va)?;
        let m = &self.modules[i];
        let off = va - m.base;
        let symbol = self.symbols[i].partition_point(|(rva, _)| (*rva as u64) <= off);
        let symbol = symbol.checked_sub(1).map(|j| {
            let (rva, name) = &self.symbols[i][j];
            (name.clone(), off - *rva as u64)
        });
        Some(Location { module: m.name.clone(), offset: off, symbol })
    }
}

fn reg_value(regs: &Registers, reg: Register) -> Option<u64> {
    let name = match reg {
        Register::RAX | Register::EAX => "RAX",
        Register::RCX | Register::ECX => "RCX",
        Register::RDX | Register::EDX => "RDX",
        Register::RBX | Register::EBX => "RBX",
        Register::RSP | Register::ESP => "RSP",
        Register::RBP | Register::EBP => "RBP",
        Register::RSI | Register::ESI => "RSI",
        Register::RDI | Register::EDI => "RDI",
        Register::R8 => "R8",
        Register::R9 => "R9",
        Register::R10 => "R10",
        Register::R11 => "R11",
        Register::R12 => "R12",
        Register::R13 => "R13",
        Register::R14 => "R14",
        Register::R15 => "R15",
        _ => return None,
    };
    regs.get(name)
}

/// Decode the instructions around the fault and annotate the faulting one.
fn disassemble(dump: &Dump, res: &Resolver, fault: u64, regs: &Registers) -> Vec<AsmRow> {
    // Decoding must start on an instruction boundary, so try a few lead-in
    // sizes (largest first) and keep the first window that decodes onto the
    // fault. Starting exactly at the fault always works when it is captured.
    let tail = 16 * (ASM_AFTER + 1);
    let mut found = None;
    for lead in [16 * ASM_BEFORE, 64, 48, 32, 16, 8, 4, 2, 1, 0] {
        let start = fault.saturating_sub(lead as u64);
        let Some(bytes) = dump.read(start, lead + tail) else { continue };
        let mut dec = Decoder::with_ip(64, bytes, start, DecoderOptions::NONE);
        let mut instrs: Vec<Instruction> = Vec::new();
        while dec.can_decode() {
            instrs.push(dec.decode());
        }
        if let Some(fi) = instrs.iter().position(|i| i.ip() == fault) {
            found = Some((instrs, fi));
            break;
        }
    }
    let Some((instrs, fi)) = found else {
        return Vec::new();
    };
    let lo = fi.saturating_sub(ASM_BEFORE);
    let hi = (fi + ASM_AFTER + 1).min(instrs.len());
    let mut fmt = IntelFormatter::new();
    let mut rows = Vec::new();
    for ins in &instrs[lo..hi] {
        let mut text = String::new();
        fmt.format(ins, &mut text);
        // Every RIP-relative read names the global it touches and its value,
        // so a NULL interface pointer stored in a global is visible even when
        // the load happens a few instructions before the fault.
        let note = if ins.ip() == fault {
            memory_note(dump, res, ins, regs)
        } else if ins.is_ip_rel_memory_operand() {
            memory_note(dump, res, ins, regs)
        } else {
            None
        };
        let target = mem_target(ins, regs);
        rows.push(AsmRow { address: ins.ip(), text, fault: ins.ip() == fault, note, target });
    }
    rows
}

/// The address an instruction's memory operand resolves to, when knowable.
fn mem_target(ins: &Instruction, regs: &Registers) -> Option<u64> {
    if !(0..ins.op_count()).any(|i| ins.op_kind(i) == OpKind::Memory) {
        return None;
    }
    if ins.is_ip_rel_memory_operand() {
        return Some(ins.ip_rel_memory_address());
    }
    let base = ins.memory_base();
    if base == Register::None {
        return None;
    }
    let v = reg_value(regs, base)?;
    Some(v.wrapping_add(ins.memory_displacement64()))
}

/// Explain the memory operand of the faulting instruction: which register
/// was NULL, or which global holds the bad pointer.
fn memory_note(dump: &Dump, res: &Resolver, ins: &Instruction, regs: &Registers) -> Option<String> {
    let mem = (0..ins.op_count()).find(|&i| ins.op_kind(i) == OpKind::Memory)?;
    let _ = mem;
    if ins.is_ip_rel_memory_operand() {
        let target = ins.ip_rel_memory_address();
        let loc = res.locate(target).map(|l| l.describe()).unwrap_or_else(|| format!("0x{target:X}"));
        let value = dump.read_u64(target);
        return Some(match value {
            Some(v) => format!("reads global {loc} = 0x{v:X}"),
            None => format!("reads global {loc} (not captured in the dump)"),
        });
    }
    let base = ins.memory_base();
    if base != Register::None {
        let v = reg_value(regs, base)?;
        let disp = ins.memory_displacement64();
        let addr = v.wrapping_add(disp);
        return Some(format!(
            "{} = 0x{:X}, dereferenced address 0x{:X}{}",
            format!("{base:?}").to_lowercase(),
            v,
            addr,
            if v == 0 { "  (NULL pointer)" } else { "" }
        ));
    }
    None
}

/// Return addresses on the stack are real only if the bytes in front of them
/// are a call instruction and that code exists in the dump.
fn is_call_site(dump: &Dump, ret: u64) -> bool {
    let Some(b) = dump.read(ret - 6, 6) else { return false };
    // call rel32 (E8 xx xx xx xx) ends exactly at `ret`
    if b[1] == 0xE8 {
        return true;
    }
    // FF /2 forms, with or without a REX.B prefix (41):
    //   call reg          FF D0..D7       (2 bytes)
    //   call [reg+disp8]  FF 50..57 d8    (3 bytes)
    //   call [rip+disp32] FF 15 d32       (6 bytes)
    (b[4] == 0xFF && (b[5] & 0xF8) == 0xD0)
        || (b[3] == 0xFF && (b[4] & 0xF8) == 0x50)
        || (b[3] == 0x41 && b[4] == 0xFF && (b[5] & 0xF8) == 0xD0)
        || (b[0] == 0xFF && b[1] == 0x15)
}

/// A region of one module: the module, its loaded symbol table (rva, name)
/// and the PDB status, so the UI can label addresses the same way the report
/// does without re-resolving the PDB.
pub struct ModuleContext {
    pub index: usize,
    pub module: Module,
    pub pdb: PdbState,
    pub symbols: Vec<(u32, String)>,
}

impl Report {
    /// Everything needed to disassemble arbitrary addresses in one module.
    pub fn module_context(&self, index: usize) -> Option<ModuleContext> {
        let row = self.modules.get(index)?;
        let symbols = match &row.pdb {
            PdbState::Loaded(n) if *n > 0 => load_pdb_by_index(self, index).unwrap_or_default(),
            _ => Vec::new(),
        };
        Some(ModuleContext { index, module: row.module.clone(), pdb: row.pdb.clone(), symbols })
    }
}

/// Re-read the symbol table for module `index` (Report keeps only rows).
fn load_pdb_by_index(report: &Report, index: usize) -> Option<Vec<(u32, String)>> {
    // The report does not carry the engine root; the UI supplies one through
    // disassemble_in_module instead.  Kept for API symmetry.
    let _ = (report, index);
    None
}

/// Disassemble `count` instructions of code captured in the dump, starting
/// near `va` (decoding walks back up to 64 bytes to find an instruction
/// boundary, as the fault view does).  Rows carry the same annotations as
/// the fault view: resolved globals, register dereferences, symbol names.
pub fn disassemble_range(
    dump_path: &Path,
    engine_root: &Path,
    module_index: usize,
    va: u64,
    count: usize,
) -> Result<(ModuleContext, Vec<AsmRow>, Option<Registers>), String> {
    let dump = Dump::open(dump_path)?;
    let modules = dump.modules();
    let module = modules
        .get(module_index)
        .ok_or_else(|| "module index out of range".to_string())?;
    let (pdb, symbols) = resolve_pdb(engine_root, module);
    let ctx = ModuleContext { index: module_index, module: module.clone(), pdb, symbols };

    let res = Resolver { modules, symbols: vec![ctx.symbols.clone()] };
    let regs = dump.exception().and_then(|e| e.registers).unwrap_or_default();

    // capture the requested window plus lead-in for boundary finding; shrink
    // the lead-in first when it crosses into memory the dump did not capture
    // (page boundaries between sections are common).
    const ROW_BYTES: u64 = 16;
    let mut tail = (count as u64) * ROW_BYTES;
    // forward tail first: shrink until the captured window fits
    let mut have = None;
    loop {
        if let Some(b) = dump.read(va, tail as usize) {
            have = Some(b.to_vec());
            break;
        }
        if tail <= ROW_BYTES {
            break;
        }
        tail -= ROW_BYTES;
    }
    // lead-in for boundary finding: shrink until read succeeds, then splice
    let mut lead = 64u64;
    let mut lead_bytes: Vec<u8> = Vec::new();
    while lead > 0 {
        if let Some(b) = dump.read(va - lead, lead as usize) {
            lead_bytes = b.to_vec();
            break;
        }
        lead -= 16;
    }
    let Some(tail_bytes) = have else {
        return Ok((ctx, Vec::new(), Some(regs)));
    };
    let mut bytes = lead_bytes;
    let start = va - lead;
    bytes.extend_from_slice(&tail_bytes);
    let mut dec = Decoder::with_ip(64, &bytes, start, DecoderOptions::NONE);
    let mut rows: Vec<AsmRow> = Vec::new();
    let mut fmt = IntelFormatter::new();
    while dec.can_decode() && rows.len() < count {
        let ins = dec.decode();
        if ins.ip() < va {
            continue; // lead-in only for boundary alignment
        }
        let mut text = String::new();
        fmt.format(&ins, &mut text);
        let note = memory_note(&dump, &res, &ins, &regs);
        let target = mem_target(&ins, &regs);
        rows.push(AsmRow { address: ins.ip(), text, fault: false, note, target });
    }
    Ok((ctx, rows, Some(regs)))
}

/// Full analysis: exception, modules with PDB status, disassembly, stack.
/// Analyse a dump. Two passes: the first places every address without
/// opening any PDB, the second opens PDBs only for the modules those
/// addresses fall in. A 1.5 GB server.pdb is therefore read only when the
/// fault or the call chain really lands in server.dll.
pub fn analyze(dump_path: &Path, engine_root: &Path) -> Result<Report, String> {
    let dump = Dump::open(dump_path)?;
    let size = std::fs::metadata(dump_path).map(|m| m.len()).unwrap_or(0);

    let first = build(&dump, dump_path, size, engine_root, Some(&HashSet::new()));
    let modules = dump.modules();
    let resolver = Resolver { modules: modules.clone(), symbols: vec![Vec::new(); modules.len()] };
    let mut needed: HashSet<usize> = HashSet::new();
    let mut mark = |addr: u64| {
        if let Some(i) = resolver.module_of(addr) {
            needed.insert(i);
        }
    };
    if let Some(e) = &first.exception {
        mark(e.address);
        if let Some(rip) = e.registers.get("RIP") {
            mark(rip);
        }
    }
    for f in &first.frames {
        mark(f.value);
    }
    for row in &first.fault_asm {
        if let Some(t) = row.target {
            mark(t);
        }
    }
    Ok(build(&dump, dump_path, size, engine_root, Some(&needed)))
}

/// One analysis pass. `load` lists the module indexes whose PDB may be
/// opened; `None` opens every PDB (used by the ignored real-file test).
fn build(
    dump: &Dump,
    dump_path: &Path,
    size: u64,
    engine_root: &Path,
    load: Option<&HashSet<usize>>,
) -> Report {
    let modules = dump.modules();

    let mut notes = Vec::new();
    let mut module_rows = Vec::new();
    let mut symbols = Vec::new();
    for (i, m) in modules.iter().enumerate() {
        let wanted = load.map(|set| set.contains(&i)).unwrap_or(true);
        let (pdb, funcs) = if wanted {
            resolve_pdb(engine_root, m)
        } else {
            (PdbState::NotLoaded, Vec::new())
        };
        symbols.push(funcs);
        module_rows.push(ModuleRow { module: m.clone(), pdb });
    }
    let resolver = Resolver { modules: modules.clone(), symbols };

    let exc = dump.exception();
    let mut exception_row = None;
    let mut fault_asm = Vec::new();
    let mut frames = Vec::new();
    let mut unverified = 0usize;

    if let Some(e) = &exc {
        let loc = resolver.locate(e.address);
        let registers = e.registers.clone().unwrap_or_default();
        if e.code == 0xC000_0005 && e.params.len() >= 2 && e.params[1] == 0 {
            notes.push(
                "access violation without a recorded target address: the crash handler did not store \
                 ExceptionInformation, so read/write and the bad address are unknown"
                    .into(),
            );
        }
        if e.registers.is_none() {
            notes.push("no exception-time register context in the dump".into());
        }
        if let Some(rip) = registers.get("RIP") {
            fault_asm = disassemble(dump, &resolver, rip, &registers);
            if fault_asm.is_empty() {
                notes.push("the code at the faulting instruction is not in the dump".into());
            }
        }
        if let Some(rsp) = registers.get("RSP") {
            for slot in 0..STACK_SLOTS {
                let at = rsp + (slot as u64) * 8;
                let Some(value) = dump.read_u64(at) else { break };
                if resolver.module_of(value).is_none() {
                    continue;
                }
                if is_call_site(dump, value) {
                    if let Some(location) = resolver.locate(value) {
                        frames.push(Frame { slot, sp: at, value, location });
                    }
                } else {
                    unverified += 1;
                }
            }
        }
        exception_row = Some(ExceptionRow {
            thread_id: e.thread_id,
            code: e.code,
            code_name: exception_code_name(e.code),
            address: e.address,
            location: loc,
            params: e.params.clone(),
            registers,
        });
    } else {
        notes.push("the dump has no exception stream (not a crash dump)".into());
    }

    if exc.is_some() && frames.is_empty() {
        notes.push(format!(
            "no return address on the stack could be verified: {unverified} candidate(s) point into \
             loaded modules but their call sites are not in the dump, so the call chain is not shown"
        ));
    }

    Report {
        dump_path: dump_path.to_string_lossy().into_owned(),
        size,
        modules: module_rows,
        exception: exception_row,
        fault_asm,
        frames,
        unverified_slots: unverified,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a dump whose captured memory is exactly `ranges` (VA, bytes),
    /// plus an optional exception at `thread` with the given register values.
    fn dump_with(ranges: &[(u64, Vec<u8>)], regs: &[(&str, u64)]) -> Dump {
        // layout: header at 0, directory at 0x20 (one entry per stream),
        // memory64 stream at 0x100, exception stream at 0x200, context at 0x300,
        // module list at 0x400, module name at 0x500, CodeView at 0x600, data from 0x1000.
        let mut d = vec![0u8; 0x1000];
        let mut streams: Vec<(u32, usize, usize)> = Vec::new();

        // memory64 list
        let n = ranges.len();
        let mut mem = Vec::new();
        mem.extend_from_slice(&(n as u64).to_le_bytes());
        mem.extend_from_slice(&0x1000u64.to_le_bytes());
        for (va, bytes) in ranges {
            mem.extend_from_slice(&va.to_le_bytes());
            mem.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        }
        streams.push((STREAM_MEMORY64_LIST, 0x100, mem.len()));
        d.resize(0x100, 0);
        d.extend_from_slice(&mem);
        d.resize(0x1000, 0);
        for (_, bytes) in ranges {
            d.extend_from_slice(bytes);
        }

        // exception stream + context
        let mut ctx = vec![0u8; 1232];
        for (name, v) in regs {
            let off = match *name {
                "RAX" => 0x78, "RCX" => 0x80, "RDX" => 0x88, "RBX" => 0x90, "RSP" => 0x98,
                "RBP" => 0xA0, "RSI" => 0xA8, "RDI" => 0xB0, "R8" => 0xB8, "R9" => 0xC0,
                "R10" => 0xC8, "R11" => 0xD0, "R12" => 0xD8, "R13" => 0xE0, "R14" => 0xE8,
                "R15" => 0xF0, "RIP" => 0xF8, other => panic!("bad reg {other}"),
            };
            ctx[off..off + 8].copy_from_slice(&v.to_le_bytes());
        }
        let ctx_at = 0x300;
        let mut exc = vec![0u8; 168];
        exc[0..4].copy_from_slice(&7u32.to_le_bytes());
        exc[8..12].copy_from_slice(&0xC000_0005u32.to_le_bytes());
        let rip = regs.iter().find(|(n, _)| *n == "RIP").map(|x| x.1).unwrap_or(0);
        exc[24..32].copy_from_slice(&rip.to_le_bytes());
        exc[32..36].copy_from_slice(&2u32.to_le_bytes());
        exc[160..164].copy_from_slice(&1232u32.to_le_bytes());
        exc[164..168].copy_from_slice(&(ctx_at as u32).to_le_bytes());
        streams.push((STREAM_EXCEPTION, 0x200, exc.len()));
        d[0x200..0x200 + exc.len()].copy_from_slice(&exc);
        d[ctx_at..ctx_at + 1232].copy_from_slice(&ctx);

        // one module: base 0x10000 (the code), name at 0x500, CodeView at 0x600
        let mut module = vec![0u8; 4 + 108];
        module[0..4].copy_from_slice(&1u32.to_le_bytes());
        let e = 4;
        module[e..e + 8].copy_from_slice(&0x10000u64.to_le_bytes());
        module[e + 8..e + 12].copy_from_slice(&0x1000u32.to_le_bytes());
        module[e + 20..e + 24].copy_from_slice(&0x500u32.to_le_bytes());
        module[e + 76..e + 80].copy_from_slice(&24u32.to_le_bytes());
        module[e + 80..e + 84].copy_from_slice(&0x600u32.to_le_bytes());
        d[0x400..0x400 + module.len()].copy_from_slice(&module);
        let name: Vec<u8> = "C:\\srv\\server.dll".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        d[0x500..0x504].copy_from_slice(&(name.len() as u32).to_le_bytes());
        d[0x504..0x504 + name.len()].copy_from_slice(&name);
        d[0x600..0x604].copy_from_slice(b"RSDS");
        d[0x604..0x614].copy_from_slice(&[1u8; 16]);
        d[0x614..0x618].copy_from_slice(&1u32.to_le_bytes());
        streams.push((STREAM_MODULE_LIST, 0x400, module.len()));

        // directory
        d[0..4].copy_from_slice(&MDMP_SIGNATURE.to_le_bytes());
        d[8..12].copy_from_slice(&(streams.len() as u32).to_le_bytes());
        d[12..16].copy_from_slice(&0x20u32.to_le_bytes());
        for (i, (t, rva, size)) in streams.iter().enumerate() {
            let at = 0x20 + i * 12;
            d[at..at + 4].copy_from_slice(&t.to_le_bytes());
            d[at + 4..at + 8].copy_from_slice(&(*size as u32).to_le_bytes());
            d[at + 8..at + 12].copy_from_slice(&(*rva as u32).to_le_bytes());
        }
        Dump::from_bytes(d).expect("synthetic dump parses")
    }

    fn code_at(base: u64, bytes: &[u8]) -> Vec<(u64, Vec<u8>)> {
        let mut page = vec![0x90u8; 0x100];
        page[..bytes.len()].copy_from_slice(bytes);
        vec![(base, page)]
    }

    #[test]
    fn parses_exception_and_module_identity() {
        let dump = dump_with(&code_at(0x10000, &[0xC3]), &[("RIP", 0x10040), ("RCX", 0)]);
        let mods = dump.modules();
        assert_eq!(mods.len(), 1);
        assert_eq!(mods[0].name, "server.dll");
        assert_eq!(mods[0].base, 0x10000);
        assert_eq!(mods[0].codeview.map(|(_, age)| age), Some(1));
        let e = dump.exception().unwrap();
        assert_eq!(e.thread_id, 7);
        assert_eq!(e.code, 0xC000_0005);
        assert_eq!(e.registers.unwrap().get("RCX"), Some(0));
    }

    #[test]
    fn rejects_non_dumps() {
        assert!(Dump::from_bytes(vec![0u8; 64]).is_err());
    }

    #[test]
    fn read_respects_captured_ranges() {
        let dump = dump_with(&code_at(0x10000, &[0xC3]), &[("RIP", 0x10000)]);
        assert!(dump.read(0x10000, 8).is_some());
        assert!(dump.read(0x10000 + 0x100, 8).is_none());
        assert!(dump.is_mapped(0x10000));
        assert!(!dump.is_mapped(0x90000));
    }

    #[test]
    fn describes_locations_with_and_without_symbols() {
        let loc = Location { module: "server.dll".into(), offset: 0x4E65A9, symbol: None };
        assert_eq!(loc.describe(), "server.dll+0x4E65A9");
        let loc = Location {
            module: "server.dll".into(),
            offset: 0x4E65A9,
            symbol: Some(("CBaseEntity::Think".into(), 0x20)),
        };
        assert_eq!(loc.describe(), "server.dll!CBaseEntity::Think+0x20");
    }

    #[test]
    fn call_sites_are_recognised_from_their_bytes() {
        // E8 rel32 ending exactly at the return address
        let e8 = dump_with(&code_at(0x10000, &[0x90, 0xE8, 0, 0, 0, 0]), &[]);
        assert!(is_call_site(&e8, 0x10006));
        // call rax (FF D0), ending at 0x10006
        let ff = dump_with(&code_at(0x10000, &[0x90, 0x90, 0x90, 0x90, 0xFF, 0xD0]), &[]);
        assert!(is_call_site(&ff, 0x10006));
        // call r8 (41 FF D0), ending at 0x10006
        let rex = dump_with(&code_at(0x10000, &[0x90, 0x90, 0x90, 0x41, 0xFF, 0xD0]), &[]);
        assert!(is_call_site(&rex, 0x10006));
        // call [rax+0x10] (FF 50 10), ending at 0x10003 needs 6 bytes before it:
        // use an address past the window start
        let mem = dump_with(&code_at(0x10000, &[0x90, 0x90, 0x90, 0xFF, 0x50, 0x10]), &[]);
        assert!(is_call_site(&mem, 0x10006));
        // plain instructions are not call sites
        let nop = dump_with(&code_at(0x10000, &[0x90; 8]), &[]);
        assert!(!is_call_site(&nop, 0x10006));
    }

    #[test]
    fn faulting_load_through_null_register_is_explained() {
        // mov rax, [rcx] at the fault, with RCX = 0
        let code = [0x48, 0x8B, 0x01, 0xC3];
        let dump = dump_with(&code_at(0x10000, &code), &[("RIP", 0x10000), ("RCX", 0), ("RAX", 1)]);
        let regs = dump.exception().unwrap().registers.unwrap();
        let res = Resolver { modules: dump.modules(), symbols: vec![Vec::new()] };
        let rows = disassemble(&dump, &res, 0x10000, &regs);
        let fault = rows.iter().find(|r| r.fault).expect("fault row");
        assert!(fault.text.starts_with("mov rax"), "got {}", fault.text);
        let note = fault.note.clone().unwrap();
        assert!(note.contains("rcx") && note.contains("NULL"), "note: {note}");
    }

    #[test]
    fn rip_relative_global_is_read_from_the_dump() {
        // mov rcx, [rip+0x10] : target = 0x10000 + 7 + 0x10 = 0x10017
        let code = [0x48, 0x8B, 0x0D, 0x10, 0x00, 0x00, 0x00, 0xC3];
        let mut page = vec![0x90u8; 0x100];
        page[..code.len()].copy_from_slice(&code);
        page[0x17..0x1F].copy_from_slice(&0u64.to_le_bytes()); // the global, NULL
        let dump = dump_with(&[(0x10000, page)], &[("RIP", 0x10000)]);
        let regs = dump.exception().unwrap().registers.unwrap();
        let res = Resolver { modules: dump.modules(), symbols: vec![Vec::new()] };
        let rows = disassemble(&dump, &res, 0x10000, &regs);
        let note = rows.iter().find(|r| r.fault).unwrap().note.clone().unwrap();
        assert!(note.contains("reads global") && note.contains("= 0x0"), "note: {note}");
        assert!(note.contains("server.dll+0x17"), "note: {note}");
    }

    #[test]
    fn addresses_resolve_to_the_nearest_function_below_them() {
        let m = Module {
            name: "server.dll".into(),
            path: "x".into(),
            base: 0x1000_0000,
            size: 0x10_0000,
            codeview: None,
        };
        let res = Resolver {
            modules: vec![m],
            symbols: vec![vec![(0x100, "alpha".into()), (0x400, "beta".into())]],
        };
        let at = |off: u64| res.locate(0x1000_0000 + off).unwrap();
        assert_eq!(at(0x120).symbol, Some(("alpha".into(), 0x20)));
        assert_eq!(at(0x400).symbol, Some(("beta".into(), 0)));
        // below the first symbol: no function name, only module+offset
        assert_eq!(at(0x40).symbol, None);
        assert_eq!(at(0x40).describe(), "server.dll+0x40");
        // outside every module
        assert!(res.locate(0x900_0000).is_none());
    }

    #[test]
    fn pdb_candidates_follow_the_binary_location() {
        let m = Module {
            name: "server.dll".into(),
            path: "D:\\srceng\\hl2sb\\bin\\server.dll".into(),
            base: 0,
            size: 0,
            codeview: None,
        };
        let root = Path::new("/games/srceng");
        let c = pdb_candidates(root, &m);
        assert_eq!(c[0], root.join("hl2sb").join("bin").join("server.pdb"));
        assert!(c.contains(&root.join("bin").join("server.pdb")));
    }

    /// Runs only when HL2BOX_PDB points at a real MSVC PDB:
    /// `HL2BOX_PDB=vpc.pdb cargo test -- --ignored real_pdb`.
    #[test]
    #[ignore]
    fn real_pdb_loads_and_rejects_other_builds() {
        let Ok(path) = std::env::var("HL2BOX_PDB") else { return };
        let path = Path::new(&path);
        let file = std::fs::File::open(path).expect("open pdb");
        let mut pdb = pdb::PDB::open(file).expect("parse pdb");
        let guid = pdb.pdb_information().expect("pdb info").guid.to_bytes_le();

        let funcs = load_pdb(path, &guid).expect("matching guid loads");
        assert!(!funcs.is_empty(), "a real PDB has public function symbols");
        assert!(funcs.windows(2).all(|w| w[0].0 <= w[1].0), "table is sorted by rva");
        eprintln!("{} functions, first: {:?}", funcs.len(), &funcs[..funcs.len().min(5)]);

        let other = [0xABu8; 16];
        assert_eq!(load_pdb(path, &other).err(), Some(PdbState::Mismatch));
    }


    #[test]
    #[ignore]
    fn real_disassemble_range() {
        let (Ok(dump), Ok(engine)) = (std::env::var("HL2BOX_DUMP"), std::env::var("HL2BOX_ENGINE")) else { return };
        let d = Dump::open(Path::new(&dump)).expect("open");
        let exc = d.exception().expect("exception");
        let rip = exc.registers.as_ref().and_then(|r| r.get("RIP")).expect("RIP");
        let mods = d.modules();
        let mi = mods.iter().position(|m| rip >= m.base && rip < m.base + m.size).expect("rip in a module");
        let (ctx, rows, regs) = disassemble_range(
            Path::new(&dump), Path::new(&engine), mi, rip, 12).expect("range");
        eprintln!("module {} pdb {:?} rows {} (at RIP 0x{rip:X})", ctx.module.name, ctx.pdb, rows.len());
        let _ = regs;
        for r in &rows { eprintln!("  0x{:X} {}", r.address, r.text); }
    }

    /// Runs only when HL2BOX_DUMP and HL2BOX_ENGINE point at real files:
    /// `HL2BOX_DUMP=x.mdmp HL2BOX_ENGINE=/games/srceng cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_dump_report() {
        let (Ok(dump), Ok(engine)) = (std::env::var("HL2BOX_DUMP"), std::env::var("HL2BOX_ENGINE")) else {
            return;
        };
        let report = analyze(Path::new(&dump), Path::new(&engine)).expect("analyze");
        eprintln!("modules: {}", report.modules.len());
        for m in &report.modules {
            eprintln!("  {:<24} {:?}", m.module.name, m.pdb);
        }
        if let Some(e) = &report.exception {
            eprintln!("exception {} at 0x{:X} {:?}", e.code_name, e.address, e.location.as_ref().map(|l| l.describe()));
        }
        for row in &report.fault_asm {
            eprintln!("{} 0x{:X}  {}{}", if row.fault { ">" } else { " " }, row.address, row.text, row.note.clone().map(|n| format!("   ; {n}")).unwrap_or_default());
        }
        for f in &report.frames {
            eprintln!("  frame {:>3} {}", f.slot, f.location.describe());
        }
        for n in &report.notes {
            eprintln!("note: {n}");
        }
    }
}
