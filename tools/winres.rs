// Shared by build.rs and installer/build.rs (pulled in with `include!`).
//
// Puts an icon (and optionally an application manifest) into the exe being
// built. No resource compiler is needed: this writes, by itself, the same
// thing `rc` + `cvtres` (or `windres`) would produce, a COFF object holding a
// `.rsrc` section, and hands it to the linker. Both link.exe and GNU ld merge
// such a section into the exe's resources.
//
// The resources are one RT_ICON per image in the .ico, the RT_GROUP_ICON
// directory that ties them together, and the RT_MANIFEST if given, under the
// usual three-level resource tree (type → id → language).

use std::collections::BTreeMap;
use std::path::PathBuf;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_MANIFEST: u16 = 24;
const LANG_EN_US: u16 = 0x0409;

/// Embeds the .ico at `icon` (and `manifest`, if any) into the bins of the
/// crate being built. Does nothing when not building for Windows.
fn embed_resources(icon: &str, manifest: Option<&str>) {
    println!("cargo:rerun-if-changed={icon}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // COFF machine and the "32-bit address relative to image base" relocation
    let (machine, reloc) = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => (0x8664u16, 3u16),
        Ok("aarch64") => (0xAA64, 2),
        Ok("x86") => (0x014C, 7),
        _ => return,
    };
    let ico = std::fs::read(icon).unwrap_or_else(|e| panic!("read {icon}: {e}"));
    let mut res = icon_resources(&ico);
    if let Some(m) = manifest {
        res.insert((RT_MANIFEST, 1), m.as_bytes().to_vec());
    }
    let obj = coff(&rsrc_section(&res), machine, reloc);
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("resources.obj");
    std::fs::write(&out, obj).expect("write resource object");
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// (type, id) → data, from an .ico file.
fn icon_resources(ico: &[u8]) -> BTreeMap<(u16, u16), Vec<u8>> {
    assert!(ico.len() >= 6 && u16_at(ico, 2) == 1, "the icon is not an .ico file");
    let count = u16_at(ico, 4) as usize;
    let mut res = BTreeMap::new();
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + i * 16;
        let size = u32_at(ico, e + 8) as usize;
        let offset = u32_at(ico, e + 12) as usize;
        let id = (i + 1) as u16;
        res.insert((RT_ICON, id), ico[offset..offset + size].to_vec());
        group.extend(&ico[e..e + 12]); // width .. bytes in resource, as in the file
        group.extend(id.to_le_bytes());
    }
    res.insert((RT_GROUP_ICON, 1), group);
    res
}

struct Section {
    data: Vec<u8>,
    /// offsets of the fields that hold a section offset to turn into an RVA
    relocs: Vec<u32>,
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// IMAGE_RESOURCE_DIRECTORY with `ids` entries; returns where the entries start.
fn directory(b: &mut [u8], at: usize, ids: usize) -> usize {
    b[at + 14..at + 16].copy_from_slice(&(ids as u16).to_le_bytes());
    at + 16
}

fn rsrc_section(res: &BTreeMap<(u16, u16), Vec<u8>>) -> Section {
    let mut types: BTreeMap<u16, Vec<(u16, &Vec<u8>)>> = BTreeMap::new();
    for ((t, id), data) in res {
        types.entry(*t).or_default().push((*id, data));
    }
    let dir = |n: usize| 16 + 8 * n;
    let align8 = |n: usize| (n + 7) & !7;

    // positions: root, type dirs, id dirs, language dirs, data entries, data
    let mut pos = dir(types.len());
    let type_dirs: Vec<usize> = types.values().map(|ids| { let p = pos; pos += dir(ids.len()); p }).collect();
    let lang_dirs: Vec<Vec<usize>> = types.values().map(|ids| ids.iter().map(|_| { let p = pos; pos += dir(1); p }).collect()).collect();
    let entries: Vec<Vec<usize>> = types.values().map(|ids| ids.iter().map(|_| { let p = pos; pos += 16; p }).collect()).collect();
    pos = align8(pos);
    let blobs: Vec<Vec<usize>> = types.values().map(|ids| ids.iter().map(|(_, d)| { let p = pos; pos = align8(pos + d.len()); p }).collect()).collect();

    let mut b = vec![0u8; pos];
    let mut relocs = Vec::new();
    const SUBDIR: u32 = 0x8000_0000;
    let mut e = directory(&mut b, 0, types.len());
    for (ti, (t, ids)) in types.iter().enumerate() {
        put32(&mut b, e, *t as u32);
        put32(&mut b, e + 4, SUBDIR | type_dirs[ti] as u32);
        e += 8;
        let mut ie = directory(&mut b, type_dirs[ti], ids.len());
        for (ii, (id, data)) in ids.iter().enumerate() {
            put32(&mut b, ie, *id as u32);
            put32(&mut b, ie + 4, SUBDIR | lang_dirs[ti][ii] as u32);
            ie += 8;
            let le = directory(&mut b, lang_dirs[ti][ii], 1);
            put32(&mut b, le, LANG_EN_US as u32);
            put32(&mut b, le + 4, entries[ti][ii] as u32); // a leaf: no high bit
            let de = entries[ti][ii];
            put32(&mut b, de, blobs[ti][ii] as u32); // OffsetToData, relocated to an RVA
            put32(&mut b, de + 4, data.len() as u32);
            relocs.push(de as u32);
            b[blobs[ti][ii]..blobs[ti][ii] + data.len()].copy_from_slice(data);
        }
    }
    Section { data: b, relocs }
}

/// A COFF object with the one `.rsrc` section and its section symbol.
fn coff(s: &Section, machine: u16, reloc_type: u16) -> Vec<u8> {
    let raw = 20 + 40;
    let relocs_at = raw + s.data.len();
    let symbols_at = relocs_at + 10 * s.relocs.len();
    let mut o = Vec::new();
    // file header
    o.extend(machine.to_le_bytes());
    o.extend(1u16.to_le_bytes()); // sections
    o.extend(0u32.to_le_bytes()); // timestamp
    o.extend((symbols_at as u32).to_le_bytes());
    o.extend(2u32.to_le_bytes()); // symbol + its aux record
    o.extend(0u16.to_le_bytes()); // optional header
    o.extend(0u16.to_le_bytes()); // characteristics
    // section header
    o.extend(*b".rsrc\0\0\0");
    o.extend(0u32.to_le_bytes());
    o.extend(0u32.to_le_bytes());
    o.extend((s.data.len() as u32).to_le_bytes());
    o.extend((raw as u32).to_le_bytes());
    o.extend((relocs_at as u32).to_le_bytes());
    o.extend(0u32.to_le_bytes());
    o.extend((s.relocs.len() as u16).to_le_bytes());
    o.extend(0u16.to_le_bytes());
    o.extend(0x4030_0040u32.to_le_bytes()); // initialized data, read, 4-byte aligned
    o.extend(&s.data);
    for r in &s.relocs {
        o.extend(r.to_le_bytes());
        o.extend(0u32.to_le_bytes()); // symbol 0: the section itself
        o.extend(reloc_type.to_le_bytes());
    }
    // symbol: .rsrc, section 1, static, one aux record
    o.extend(*b".rsrc\0\0\0");
    o.extend(0u32.to_le_bytes());
    o.extend(1i16.to_le_bytes());
    o.extend(0u16.to_le_bytes());
    o.push(3);
    o.push(1);
    // aux: section length, relocation count
    o.extend((s.data.len() as u32).to_le_bytes());
    o.extend((s.relocs.len() as u16).to_le_bytes());
    o.extend([0u8; 12]);
    // empty string table
    o.extend(4u32.to_le_bytes());
    o
}
