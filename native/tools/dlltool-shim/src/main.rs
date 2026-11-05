// Minimal stand-in for binutils dlltool (x86_64 only): builds a GNU-style import library
// (head object, one thunk object per symbol, tail object) from a .def file, without needing `as`.
use std::io::Write;

struct Sym {
    name: String,
    value: u32,
    sect: i16,
    class: u8,
    func: bool,
}
struct Sec {
    name: &'static str,
    data: Vec<u8>,
    relocs: Vec<(u32, usize, u16)>,
    flags: u32,
}

const DATA_FLAGS: u32 = 0xC030_0040;
const HINT_FLAGS: u32 = 0xC020_0040;
const TEXT_FLAGS: u32 = 0x6050_0020;
const ADDR32NB: u16 = 3;
const REL32: u16 = 4;

fn coff(secs: &[Sec], syms: &[Sym]) -> Vec<u8> {
    let mut strtab: Vec<u8> = vec![0; 4];
    let mut sym_bytes = Vec::new();
    for s in syms {
        if s.name.len() <= 8 {
            let mut n = [0u8; 8];
            n[..s.name.len()].copy_from_slice(s.name.as_bytes());
            sym_bytes.extend(n);
        } else {
            sym_bytes.extend(0u32.to_le_bytes());
            sym_bytes.extend((strtab.len() as u32).to_le_bytes());
            strtab.extend(s.name.as_bytes());
            strtab.push(0);
        }
        sym_bytes.extend(s.value.to_le_bytes());
        sym_bytes.extend(s.sect.to_le_bytes());
        sym_bytes.extend((if s.func { 0x20u16 } else { 0 }).to_le_bytes());
        sym_bytes.push(s.class);
        sym_bytes.push(0);
    }
    let l = strtab.len() as u32;
    strtab[..4].copy_from_slice(&l.to_le_bytes());

    let hdr = 20 + 40 * secs.len();
    let mut off = hdr;
    let mut raw_off = vec![];
    let mut rel_off = vec![];
    for s in secs {
        raw_off.push(if s.data.is_empty() { 0 } else { off });
        off += s.data.len();
        rel_off.push(if s.relocs.is_empty() { 0 } else { off });
        off += 10 * s.relocs.len();
    }
    let mut o = vec![];
    o.extend(0x8664u16.to_le_bytes());
    o.extend((secs.len() as u16).to_le_bytes());
    o.extend(0u32.to_le_bytes());
    o.extend((off as u32).to_le_bytes());
    o.extend((syms.len() as u32).to_le_bytes());
    o.extend(0u16.to_le_bytes());
    o.extend(0u16.to_le_bytes());
    for (i, s) in secs.iter().enumerate() {
        let mut n = [0u8; 8];
        n[..s.name.len()].copy_from_slice(s.name.as_bytes());
        o.extend(n);
        o.extend(0u32.to_le_bytes());
        o.extend(0u32.to_le_bytes());
        o.extend((s.data.len() as u32).to_le_bytes());
        o.extend((raw_off[i] as u32).to_le_bytes());
        o.extend((rel_off[i] as u32).to_le_bytes());
        o.extend(0u32.to_le_bytes());
        o.extend((s.relocs.len() as u16).to_le_bytes());
        o.extend(0u16.to_le_bytes());
        o.extend(s.flags.to_le_bytes());
    }
    for s in secs {
        o.extend(&s.data);
        for (va, si, ty) in &s.relocs {
            o.extend(va.to_le_bytes());
            o.extend((*si as u32).to_le_bytes());
            o.extend(ty.to_le_bytes());
        }
    }
    o.extend(sym_bytes);
    o.extend(strtab);
    o
}

fn sym(name: &str, value: u32, sect: i16, class: u8, func: bool) -> Sym {
    Sym { name: name.to_string(), value, sect, class, func }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (mut def, mut dll, mut out, mut prefix) = (String::new(), String::new(), String::new(), String::new());
    let mut i = 0;
    while i < a.len() {
        match a[i].as_str() {
            "-d" | "--input-def" => { def = a[i + 1].clone(); i += 1 }
            "-D" | "--dllname" => { dll = a[i + 1].clone(); i += 1 }
            "-l" | "--output-lib" => { out = a[i + 1].clone(); i += 1 }
            "--temp-prefix" => { prefix = a[i + 1].clone(); i += 1 }
            "-m" => { i += 1 }
            _ => {}
        }
        i += 1;
    }
    let text = std::fs::read_to_string(&def).expect("def");
    let mut exports: Vec<(String, bool)> = vec![];
    let mut in_exports = false;
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with(';') { continue; }
        if l.eq_ignore_ascii_case("EXPORTS") { in_exports = true; continue; }
        if l.starts_with("LIBRARY") { if dll.is_empty() { dll = l[7..].trim().trim_matches('"').to_string(); } continue; }
        if !in_exports { continue; }
        let toks: Vec<&str> = l.split_whitespace().collect();
        let name = toks[0].trim_matches('"').to_string();
        let data = toks.iter().skip(1).any(|t| t.eq_ignore_ascii_case("DATA"));
        exports.push((name, data));
    }
    if prefix.is_empty() { prefix = dll.clone(); }
    let id: String = dll.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let head_sym = format!("__head_{id}");
    let iname_sym = format!("__{id}_iname");

    // (member name, bytes, global symbols)
    let mut members: Vec<(String, Vec<u8>, Vec<String>)> = vec![];

    // head: import descriptor
    let head = coff(
        &[
            Sec { name: ".idata$2", data: vec![0; 20], relocs: vec![(0, 1, ADDR32NB), (12, 3, ADDR32NB), (16, 2, ADDR32NB)], flags: DATA_FLAGS },
            Sec { name: ".idata$5", data: vec![], relocs: vec![], flags: DATA_FLAGS },
            Sec { name: ".idata$4", data: vec![], relocs: vec![], flags: DATA_FLAGS },
        ],
        &[
            sym(&head_sym, 0, 1, 2, false),
            sym("hname", 0, 3, 3, false),
            sym("fthunk", 0, 2, 3, false),
            sym(&iname_sym, 0, 0, 2, false),
        ],
    );
    members.push((format!("{prefix}h.o"), head, vec![head_sym.clone()]));

    for (n, (name, data)) in exports.iter().enumerate() {
        let imp = format!("__imp_{name}");
        let mut hint = vec![0u8, 0u8];
        hint.extend(name.as_bytes());
        hint.push(0);
        if hint.len() % 2 == 1 { hint.push(0); }
        let mut secs = vec![];
        let mut syms: Vec<Sym> = vec![];
        let mut globals = vec![];
        // symbol layout: 0 = ID2, 1 = head (undef), 2 = __imp_X, [3 = X]
        syms.push(sym("ID2", 0, 0, 3, false));
        syms.push(sym(&head_sym, 0, 0, 2, false));
        if !*data {
            // .text thunk: jmp *__imp_X(%rip)
            secs.push(Sec { name: ".text", data: vec![0xFF, 0x25, 0, 0, 0, 0], relocs: vec![(2, 2, REL32)], flags: TEXT_FLAGS });
        }
        let base = secs.len() as i16;
        let (s5, s6) = (base + 2, base + 4);
        syms.push(sym(&imp, 0, s5, 2, false));
        globals.push(imp.clone());
        if !*data {
            syms.push(sym(name, 0, 1, 2, true));
            globals.push(name.clone());
        }
        syms[0].sect = s6;
        secs.push(Sec { name: ".idata$7", data: vec![0; 4], relocs: vec![(0, 1, ADDR32NB)], flags: DATA_FLAGS });
        secs.push(Sec { name: ".idata$5", data: vec![0; 8], relocs: vec![(0, 0, ADDR32NB)], flags: DATA_FLAGS });
        secs.push(Sec { name: ".idata$4", data: vec![0; 8], relocs: vec![(0, 0, ADDR32NB)], flags: DATA_FLAGS });
        secs.push(Sec { name: ".idata$6", data: hint, relocs: vec![], flags: HINT_FLAGS });
        members.push((format!("{prefix}s{:05}.o", n + 1), coff(&secs, &syms), globals));
    }

    let mut name_bytes = dll.as_bytes().to_vec();
    name_bytes.push(0);
    let tail = coff(
        &[
            Sec { name: ".idata$4", data: vec![0; 8], relocs: vec![], flags: DATA_FLAGS },
            Sec { name: ".idata$5", data: vec![0; 8], relocs: vec![], flags: DATA_FLAGS },
            Sec { name: ".idata$7", data: name_bytes, relocs: vec![], flags: DATA_FLAGS },
        ],
        &[sym(&iname_sym, 0, 3, 2, false)],
    );
    members.push((format!("{prefix}t.o"), tail, vec![iname_sym.clone()]));

    // long-name table
    let mut longnames: Vec<u8> = vec![];
    let mut name_field: Vec<String> = vec![];
    for (n, _, _) in &members {
        if n.len() < 16 {
            name_field.push(format!("{n}/"));
        } else {
            name_field.push(format!("/{}", longnames.len()));
            longnames.extend(n.as_bytes());
            longnames.extend(b"/\n");
        }
    }
    let all: Vec<&String> = members.iter().flat_map(|m| m.2.iter()).collect();
    let strsz: usize = all.iter().map(|s| s.len() + 1).sum();
    let sym_size = 4 + 4 * all.len() + strsz;
    let pad = |n: usize| n + (n & 1);
    let mut base = 8 + 60 + pad(sym_size);
    if !longnames.is_empty() {
        base += 60 + pad(longnames.len());
    }
    let mut offs = vec![];
    for m in &members {
        offs.push(base);
        base += 60 + pad(m.1.len());
    }

    let hdr = |name: &str, size: usize| -> Vec<u8> {
        format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", name, 0, 0, 0, 0, size).into_bytes()
    };
    let mut f = vec![];
    f.extend(b"!<arch>\n");
    f.extend(hdr("/", sym_size));
    f.extend((all.len() as u32).to_be_bytes());
    for (mi, m) in members.iter().enumerate() {
        for _ in &m.2 {
            f.extend((offs[mi] as u32).to_be_bytes());
        }
    }
    for s in &all {
        f.extend(s.as_bytes());
        f.push(0);
    }
    if sym_size & 1 == 1 {
        f.push(b'\n');
    }
    if !longnames.is_empty() {
        f.extend(hdr("//", longnames.len()));
        f.extend(&longnames);
        if longnames.len() & 1 == 1 {
            f.push(b'\n');
        }
    }
    for (mi, m) in members.iter().enumerate() {
        f.extend(hdr(&name_field[mi], m.1.len()));
        f.extend(&m.1);
        if m.1.len() & 1 == 1 {
            f.push(b'\n');
        }
    }
    std::fs::File::create(&out).unwrap().write_all(&f).unwrap();
}
