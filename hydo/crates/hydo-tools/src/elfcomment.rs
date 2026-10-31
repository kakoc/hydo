//! `hydo-elf-comment`: sets the linker's line in an ELF file's `.comment` section to a fixed text.
//! rust-lld writes "Linker: LLD <version> (<where it was built>)", and the Linux and the Mac
//! toolchains of one Rust release differ in the part in parentheses, so the same static program
//! linked on the two hosts differs only there. The section is not loaded; it and every section
//! after it in the file (the section-name table) are rewritten, the section headers are moved and
//! fixed. 64-bit little-endian ELF only (the static guest programs).

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

const SHF_ALLOC: u64 = 2;

/// Rewrites `elf` with the `.comment` entry that starts with "Linker:" replaced by `line`.
/// Returns the file unchanged when it has no such entry.
pub fn fix(elf: &[u8], line: &str) -> Result<Vec<u8>, String> {
    if elf.len() < 64 || &elf[..4] != b"\x7fELF" || elf[4] != 2 || elf[5] != 1 {
        return Err("not a 64-bit little-endian ELF file".into());
    }
    let shoff = u64le(elf, 0x28) as usize;
    let shentsize = u16le(elf, 0x3a) as usize;
    let shnum = u16le(elf, 0x3c) as usize;
    let shstrndx = u16le(elf, 0x3e) as usize;
    if shentsize != 64 || shoff + shnum * 64 > elf.len() || shstrndx >= shnum {
        return Err("bad section header table".into());
    }
    struct Sh {
        raw: Vec<u8>,
        name: u32,
        typ: u32,
        flags: u64,
        off: u64,
        size: u64,
        align: u64,
    }
    let mut shs: Vec<Sh> = (0..shnum)
        .map(|i| {
            let o = shoff + i * 64;
            Sh {
                raw: elf[o..o + 64].to_vec(),
                name: u32le(elf, o),
                typ: u32le(elf, o + 4),
                flags: u64le(elf, o + 8),
                off: u64le(elf, o + 0x18),
                size: u64le(elf, o + 0x20),
                align: u64le(elf, o + 0x30),
            }
        })
        .collect();
    let strtab = &shs[shstrndx];
    let names = elf
        .get(strtab.off as usize..(strtab.off + strtab.size) as usize)
        .ok_or("bad .shstrtab")?;
    let name_of = |n: u32| -> &[u8] {
        let s = &names[n as usize..];
        &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]
    };
    let Some(ci) = shs.iter().position(|s| name_of(s.name) == b".comment") else {
        return Ok(elf.to_vec());
    };
    let c = &shs[ci];
    let old = &elf[c.off as usize..(c.off + c.size) as usize];
    let mut entries: Vec<&[u8]> = old.split(|&b| b == 0).filter(|e| !e.is_empty()).collect();
    let mut changed = false;
    for e in entries.iter_mut() {
        if e.starts_with(b"Linker:") {
            *e = line.as_bytes();
            changed = true;
        }
    }
    if !changed {
        return Ok(elf.to_vec());
    }
    // .comment starts with a NUL (as lld writes it) and ends each entry with one
    let mut new = vec![0u8];
    for e in entries {
        new.extend_from_slice(e);
        new.push(0);
    }
    let cut = c.off as usize;
    // every section that lies at or after .comment in the file must be one that is not loaded
    for s in &shs {
        if s.off as usize >= cut && s.typ != 8 && s.flags & SHF_ALLOC != 0 {
            return Err("a loaded section follows .comment".into());
        }
    }
    if shoff < cut {
        return Err("the section headers come before .comment".into());
    }
    let mut out = elf[..cut].to_vec();
    // the sections at or after .comment, in file order, rewritten
    let mut tail: Vec<usize> = (0..shnum)
        .filter(|&i| shs[i].off as usize >= cut && shs[i].typ != 8 && shs[i].size > 0)
        .collect();
    tail.sort_by_key(|&i| shs[i].off);
    for i in tail {
        let align = shs[i].align.max(1) as usize;
        while !out.len().is_multiple_of(align) {
            out.push(0);
        }
        let data = if i == ci {
            new.clone()
        } else {
            elf[shs[i].off as usize..(shs[i].off + shs[i].size) as usize].to_vec()
        };
        shs[i].off = out.len() as u64;
        shs[i].size = data.len() as u64;
        out.extend_from_slice(&data);
    }
    while !out.len().is_multiple_of(8) {
        out.push(0);
    }
    let new_shoff = out.len() as u64;
    for s in &mut shs {
        s.raw[0x18..0x20].copy_from_slice(&s.off.to_le_bytes());
        s.raw[0x20..0x28].copy_from_slice(&s.size.to_le_bytes());
        out.extend_from_slice(&s.raw);
    }
    out[0x28..0x30].copy_from_slice(&new_shoff.to_le_bytes());
    Ok(out)
}
