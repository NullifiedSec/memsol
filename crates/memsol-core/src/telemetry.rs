use std::{collections::HashMap, fs, io, path::Path};

use crate::model::{MemorySnapshot, PsiLine, PsiMemory};

pub fn read_memory_psi() -> io::Result<PsiMemory> {
    let raw = fs::read_to_string("/proc/pressure/memory")?;
    parse_memory_psi(&raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid memory PSI data"))
}

pub fn read_meminfo() -> io::Result<MemorySnapshot> {
    let raw = fs::read_to_string(Path::new("/proc/meminfo"))?;
    parse_meminfo(&raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid /proc/meminfo"))
}

#[must_use]
pub fn parse_memory_psi(raw: &str) -> Option<PsiMemory> {
    let mut some = None;
    let mut full = None;

    for line in raw.lines() {
        let mut fields = line.split_whitespace();
        let kind = fields.next()?;
        let parsed = parse_psi_fields(fields)?;
        match kind {
            "some" => some = Some(parsed),
            "full" => full = Some(parsed),
            _ => {}
        }
    }

    Some(PsiMemory {
        some: some?,
        full: full?,
    })
}

fn parse_psi_fields<'a>(fields: impl Iterator<Item = &'a str>) -> Option<PsiLine> {
    let values: HashMap<&str, &str> = fields.filter_map(|field| field.split_once('=')).collect();

    Some(PsiLine {
        avg10: values.get("avg10")?.parse().ok()?,
        avg60: values.get("avg60")?.parse().ok()?,
        avg300: values.get("avg300")?.parse().ok()?,
        total_us: values.get("total")?.parse().ok()?,
    })
}

#[must_use]
pub fn parse_meminfo(raw: &str) -> Option<MemorySnapshot> {
    let mut values = HashMap::<&str, u64>::new();

    for line in raw.lines() {
        let (key, value) = line.split_once(':')?;
        let kib = value.split_whitespace().next()?.parse().ok()?;
        values.insert(key, kib);
    }

    Some(MemorySnapshot {
        total_kib: *values.get("MemTotal")?,
        available_kib: *values.get("MemAvailable")?,
        swap_total_kib: *values.get("SwapTotal")?,
        swap_free_kib: *values.get("SwapFree")?,
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_meminfo, parse_memory_psi};

    #[test]
    fn parses_memory_psi() {
        let psi = parse_memory_psi(
            "some avg10=0.15 avg60=0.10 avg300=0.05 total=12345\nfull avg10=0.01 avg60=0.00 avg300=0.00 total=900\n",
        )
        .expect("valid PSI");

        assert_eq!(psi.some.avg10, 0.15);
        assert_eq!(psi.full.total_us, 900);
    }

    #[test]
    fn parses_meminfo() {
        let snapshot = parse_meminfo(
            "MemTotal:       16384000 kB\nMemAvailable:    8192000 kB\nSwapTotal:       8388608 kB\nSwapFree:        6291456 kB\n",
        )
        .expect("valid meminfo");

        assert_eq!(snapshot.total_kib, 16_384_000);
        assert_eq!(snapshot.available_kib, 8_192_000);
        assert_eq!(snapshot.swap_used_kib(), 2_097_152);
    }
}
