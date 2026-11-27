//! The guest kernel's BPF Type Format (BTF): the layouts of its structures, which a kernel built
//! with `CONFIG_DEBUG_INFO_BTF` keeps in its own image between `__start_BTF` and `__stop_BTF`. hydo
//! reads it from guest memory, so the offsets of `task_struct` and the rest are the running
//! kernel's own.

use std::collections::HashMap;

const KIND_INT: u32 = 1;
const KIND_PTR: u32 = 2;
const KIND_ARRAY: u32 = 3;
const KIND_STRUCT: u32 = 4;
const KIND_UNION: u32 = 5;
const KIND_ENUM: u32 = 6;
const KIND_TYPEDEF: u32 = 8;
const KIND_VOLATILE: u32 = 9;
const KIND_CONST: u32 = 10;
const KIND_RESTRICT: u32 = 11;
const KIND_FUNC_PROTO: u32 = 13;
const KIND_VAR: u32 = 14;
const KIND_DATASEC: u32 = 15;
const KIND_DECL_TAG: u32 = 17;
const KIND_TYPE_TAG: u32 = 18;
const KIND_ENUM64: u32 = 19;

#[derive(Clone, Debug)]
struct Member {
    name: String,
    ty: u32,
    /// In bits.
    bit_off: u32,
    /// The bitfield's size in bits (0: not a bitfield).
    bits: u32,
}

#[derive(Clone, Debug)]
struct Ty {
    kind: u32,
    name: String,
    /// The size of an int, struct, union or enum; the target of the others.
    size_or_type: u32,
    members: Vec<Member>,
    /// An array's element type and count.
    array: Option<(u32, u32)>,
}

/// The kernel's types.
pub struct Btf {
    types: Vec<Ty>,
    by_name: HashMap<String, Vec<u32>>,
}

/// A member's place: its byte offset, its size in bytes (of the type, or of the bitfield's
/// storage unit), and for a bitfield its bit offset within the bytes and width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub off: u64,
    pub size: u64,
    pub bits: Option<(u32, u32)>,
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

impl Btf {
    pub fn parse(b: &[u8]) -> Result<Btf, String> {
        if b.get(0..2) != Some(&[0x9f, 0xeb]) {
            return Err("no BTF magic (the kernel has no CONFIG_DEBUG_INFO_BTF?)".into());
        }
        let bad = || "a damaged BTF header".to_string();
        let hdr = u32_at(b, 4).ok_or_else(bad)? as usize;
        let (toff, tlen) = (
            u32_at(b, 8).ok_or_else(bad)? as usize,
            u32_at(b, 12).ok_or_else(bad)? as usize,
        );
        let (soff, slen) = (
            u32_at(b, 16).ok_or_else(bad)? as usize,
            u32_at(b, 20).ok_or_else(bad)? as usize,
        );
        let tb = b.get(hdr + toff..hdr + toff + tlen).ok_or_else(bad)?;
        let sb = b.get(hdr + soff..hdr + soff + slen).ok_or_else(bad)?;
        let name = |o: u32| -> String {
            let s = sb.get(o as usize..).unwrap_or(&[]);
            let e = s.iter().position(|&c| c == 0).unwrap_or(s.len());
            String::from_utf8_lossy(&s[..e]).into_owned()
        };
        // type id 0 is void
        let mut types = vec![Ty {
            kind: 0,
            name: String::new(),
            size_or_type: 0,
            members: Vec::new(),
            array: None,
        }];
        let mut p = 0usize;
        while p + 12 <= tb.len() {
            let name_off = u32_at(tb, p).ok_or_else(bad)?;
            let info = u32_at(tb, p + 4).ok_or_else(bad)?;
            let st = u32_at(tb, p + 8).ok_or_else(bad)?;
            p += 12;
            let vlen = (info & 0xffff) as usize;
            let kind = (info >> 24) & 0x1f;
            let kflag = info >> 31 == 1;
            let mut t = Ty {
                kind,
                name: name(name_off),
                size_or_type: st,
                members: Vec::new(),
                array: None,
            };
            match kind {
                KIND_INT | KIND_VAR | KIND_DECL_TAG => p += 4,
                KIND_ARRAY => {
                    t.array = Some((
                        u32_at(tb, p).ok_or_else(bad)?,
                        u32_at(tb, p + 8).ok_or_else(bad)?,
                    ));
                    p += 12;
                }
                KIND_STRUCT | KIND_UNION => {
                    for i in 0..vlen {
                        let q = p + i * 12;
                        let off = u32_at(tb, q + 8).ok_or_else(bad)?;
                        let (bit_off, bits) = if kflag {
                            (off & 0x00ff_ffff, off >> 24)
                        } else {
                            (off, 0)
                        };
                        t.members.push(Member {
                            name: name(u32_at(tb, q).ok_or_else(bad)?),
                            ty: u32_at(tb, q + 4).ok_or_else(bad)?,
                            bit_off,
                            bits,
                        });
                    }
                    p += vlen * 12;
                }
                KIND_ENUM | KIND_FUNC_PROTO => p += vlen * 8,
                KIND_DATASEC | KIND_ENUM64 => p += vlen * 12,
                KIND_PTR | 7 | KIND_TYPEDEF | KIND_VOLATILE | KIND_CONST | KIND_RESTRICT | 12
                | 16 | KIND_TYPE_TAG => {}
                k => return Err(format!("BTF kind {k} unknown")),
            }
            types.push(t);
        }
        let mut by_name: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, t) in types.iter().enumerate() {
            if !t.name.is_empty() {
                by_name.entry(t.name.clone()).or_default().push(i as u32);
            }
        }
        Ok(Btf { types, by_name })
    }

    /// The type behind typedefs and qualifiers.
    fn resolve(&self, mut id: u32) -> u32 {
        for _ in 0..32 {
            match self.types.get(id as usize).map(|t| t.kind) {
                Some(KIND_TYPEDEF | KIND_VOLATILE | KIND_CONST | KIND_RESTRICT | KIND_TYPE_TAG) => {
                    id = self.types[id as usize].size_or_type
                }
                _ => break,
            }
        }
        id
    }

    /// The size in bytes of type `id`.
    fn size(&self, id: u32) -> u64 {
        let id = self.resolve(id);
        let Some(t) = self.types.get(id as usize) else {
            return 0;
        };
        match t.kind {
            KIND_INT | KIND_STRUCT | KIND_UNION | KIND_ENUM | KIND_ENUM64 => t.size_or_type as u64,
            KIND_PTR => 8,
            KIND_ARRAY => t.array.map(|(e, n)| self.size(e) * n as u64).unwrap_or(0),
            _ => 0,
        }
    }

    /// The struct (or union) of this name with members (not a forward declaration).
    fn record(&self, name: &str) -> Option<u32> {
        self.by_name.get(name)?.iter().copied().find(|&i| {
            let t = &self.types[i as usize];
            matches!(t.kind, KIND_STRUCT | KIND_UNION) && !t.members.is_empty()
        })
    }

    /// The size of struct `name`.
    pub fn struct_size(&self, name: &str) -> Option<u64> {
        Some(self.types[self.record(name)? as usize].size_or_type as u64)
    }

    /// A member of `id` by name, looking into anonymous structures and unions; its bit offset
    /// from `id`'s start and the member.
    fn find(&self, id: u32, name: &str, depth: u32) -> Option<(u64, Member)> {
        if depth > 8 {
            return None;
        }
        let t = self.types.get(self.resolve(id) as usize)?;
        for m in &t.members {
            if m.name == name {
                return Some((m.bit_off as u64, m.clone()));
            }
        }
        for m in &t.members {
            if m.name.is_empty() {
                if let Some((o, x)) = self.find(m.ty, name, depth + 1) {
                    return Some((m.bit_off as u64 + o, x));
                }
            }
        }
        None
    }

    /// The place of `path` (`thread.sp`, `d_name.name`) in struct `strukt`.
    pub fn member(&self, strukt: &str, path: &str) -> Option<Place> {
        let mut id = self.record(strukt)?;
        let mut bits = 0u64;
        let mut last = None;
        for part in path.split('.') {
            let (o, m) = self.find(id, part, 0)?;
            bits += o;
            id = m.ty;
            last = Some(m);
        }
        let m = last?;
        if m.bits > 0 {
            let unit = self.size(m.ty).max(1);
            let byte = (bits / 8) & !(unit - 1);
            return Some(Place {
                off: byte,
                size: unit,
                bits: Some(((bits - byte * 8) as u32, m.bits)),
            });
        }
        Some(Place {
            off: bits / 8,
            size: self.size(m.ty),
            bits: None,
        })
    }

    /// The byte offset of `path` in `strukt`.
    pub fn off(&self, strukt: &str, path: &str) -> Option<u64> {
        self.member(strukt, path).map(|p| p.off)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A small BTF blob: `struct inner { int a; int b; }`, `struct outer { int x; struct { struct
    /// inner in; }; unsigned int f:3, g:5; }` and a typedef.
    pub fn blob() -> Vec<u8> {
        let mut strs = vec![0u8];
        let mut s = |x: &str| {
            let o = strs.len() as u32;
            strs.extend_from_slice(x.as_bytes());
            strs.push(0);
            o
        };
        let (int, inner, a, bb, outer, x, inn, f, g, td) = (
            s("int"),
            s("inner"),
            s("a"),
            s("b"),
            s("outer"),
            s("x"),
            s("in"),
            s("f"),
            s("g"),
            s("u32t"),
        );
        let mut t: Vec<u8> = Vec::new();
        let mut w = |v: u32| t.extend_from_slice(&v.to_le_bytes());
        // 1: int (4 bytes)
        w(int);
        w(KIND_INT << 24);
        w(4);
        w(32);
        // 2: struct inner { int a; int b; }
        w(inner);
        w((KIND_STRUCT << 24) | 2);
        w(8);
        w(a);
        w(1);
        w(0);
        w(bb);
        w(1);
        w(32);
        // 3: anonymous struct { struct inner in; }
        w(0);
        w((KIND_STRUCT << 24) | 1);
        w(8);
        w(inn);
        w(2);
        w(0);
        // 4: typedef u32t = int
        w(td);
        w(KIND_TYPEDEF << 24);
        w(1);
        // 5: struct outer { int x; anon; u32t f:3; u32t g:5 } with kind_flag
        w(outer);
        w((1 << 31) | (KIND_STRUCT << 24) | 4);
        w(20);
        w(x);
        w(1);
        w(0);
        w(0);
        w(3);
        w(32);
        w(f);
        w(4);
        w((3 << 24) | 128);
        w(g);
        w(4);
        w((5 << 24) | 131);
        let mut b = Vec::new();
        b.extend_from_slice(&[0x9f, 0xeb, 1, 0]);
        b.extend_from_slice(&24u32.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&(t.len() as u32).to_le_bytes());
        b.extend_from_slice(&(t.len() as u32).to_le_bytes());
        b.extend_from_slice(&(strs.len() as u32).to_le_bytes());
        b.extend_from_slice(&t);
        b.extend_from_slice(&strs);
        b
    }

    #[test]
    fn members_through_anonymous_structs_and_bitfields() {
        let b = Btf::parse(&blob()).unwrap();
        assert_eq!(b.struct_size("outer"), Some(20));
        assert_eq!(b.off("outer", "x"), Some(0));
        assert_eq!(b.off("outer", "in"), Some(4));
        assert_eq!(b.off("outer", "in.b"), Some(8));
        assert_eq!(b.member("outer", "in.b").unwrap().size, 4);
        assert_eq!(
            b.member("outer", "f"),
            Some(Place {
                off: 16,
                size: 4,
                bits: Some((0, 3))
            })
        );
        assert_eq!(
            b.member("outer", "g"),
            Some(Place {
                off: 16,
                size: 4,
                bits: Some((3, 5))
            })
        );
        assert_eq!(b.off("outer", "nope"), None);
        assert!(Btf::parse(b"xx").is_err());
    }
}
