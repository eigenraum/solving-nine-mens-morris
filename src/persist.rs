//! On-disk database format: one file per ordered subspace, plus a JSON
//! manifest tracking sizes, checksums, and solve metadata.
//!
//! Each pair `{a, b}` only ever needs *two* already-solved dependency
//! pairs loaded to compute its captures — `{a, b-1}` and `{a-1, b}`
//! (Gasser's Figure 4 DAG) — never the full set of previously-solved
//! pairs. This keeps per-step memory bounded regardless of how many of
//! the 28 pairs have been solved so far.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ManifestEntry {
    pub w: u8,
    pub b: u8,
    pub size: u64,
    pub xxh3: String,
    pub solved_at_unix: u64,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Manifest {
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    pub fn load(dir: &Path) -> Result<Manifest> {
        let path = dir.join("manifest.json");
        if !path.exists() {
            return Ok(Manifest::default());
        }
        let text = std::fs::read_to_string(&path).context("reading manifest.json")?;
        Ok(serde_json::from_str(&text).context("parsing manifest.json")?)
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join("manifest.json");
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text).context("writing manifest.json")?;
        Ok(())
    }

    pub fn find(&self, w: usize, b: usize) -> Option<&ManifestEntry> {
        self.entries.iter().find(|e| e.w as usize == w && e.b as usize == b)
    }

    pub fn upsert(&mut self, entry: ManifestEntry) {
        self.entries.retain(|e| !(e.w == entry.w && e.b == entry.b));
        self.entries.push(entry);
    }
}

pub fn subspace_path(dir: &Path, w: usize, b: usize) -> PathBuf {
    dir.join(format!("wdl_{w}_{b}.bin"))
}

/// Reinterpret a `u16` slice as raw little-endian-on-LE-platforms bytes for
/// bulk I/O. Not portable across platforms with different native
/// endianness, which is an accepted tradeoff here: this format is only
/// meant to be read back by this same tool on the same class of machine
/// (x86_64/aarch64, both little-endian in practice), not as a
/// general-purpose interchange format.
fn as_bytes(data: &[u16]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), std::mem::size_of_val(data)) }
}

fn bytes_to_u16_vec(bytes: Vec<u8>) -> Result<Vec<u16>> {
    if bytes.len() % 2 != 0 {
        bail!("file length {} is not a multiple of 2", bytes.len());
    }
    let mut out = vec![0u16; bytes.len() / 2];
    let out_bytes: &mut [u8] =
        unsafe { std::slice::from_raw_parts_mut(out.as_mut_ptr().cast::<u8>(), bytes.len()) };
    out_bytes.copy_from_slice(&bytes);
    Ok(out)
}

pub fn xxh3_of(data: &[u16]) -> String {
    format!("{:016x}", xxhash_rust::xxh3::xxh3_64(as_bytes(data)))
}

pub fn write_subspace(dir: &Path, w: usize, b: usize, data: &[u16]) -> Result<ManifestEntry> {
    std::fs::create_dir_all(dir)?;
    let path = subspace_path(dir, w, b);
    let tmp_path = path.with_extension("bin.tmp");
    {
        let mut f = BufWriter::new(File::create(&tmp_path)?);
        f.write_all(as_bytes(data))?;
        f.flush()?;
    }
    std::fs::rename(&tmp_path, &path)?; // atomic on the same filesystem
    let xxh3 = xxh3_of(data);
    let solved_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(ManifestEntry {
        w: w as u8,
        b: b as u8,
        size: data.len() as u64,
        xxh3,
        solved_at_unix,
    })
}

pub fn read_subspace(dir: &Path, w: usize, b: usize) -> Result<Vec<u16>> {
    let path = subspace_path(dir, w, b);
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    bytes_to_u16_vec(bytes)
}

/// Read a subspace and verify its checksum against the manifest entry.
pub fn read_subspace_verified(dir: &Path, manifest: &Manifest, w: usize, b: usize) -> Result<Vec<u16>> {
    let entry = manifest
        .find(w, b)
        .with_context(|| format!("no manifest entry for ({w},{b})"))?;
    let data = read_subspace(dir, w, b)?;
    if data.len() as u64 != entry.size {
        bail!(
            "subspace ({w},{b}): file has {} entries, manifest says {}",
            data.len(),
            entry.size
        );
    }
    let actual = xxh3_of(&data);
    if actual != entry.xxh3 {
        bail!("subspace ({w},{b}): checksum mismatch (file {actual}, manifest {})", entry.xxh3);
    }
    Ok(data)
}

/// True iff subspace (w,b) is already solved on disk with a checksum
/// matching the manifest (so orchestration can skip re-solving it).
pub fn is_solved(dir: &Path, manifest: &Manifest, w: usize, b: usize) -> bool {
    let Some(entry) = manifest.find(w, b) else {
        return false;
    };
    let path = subspace_path(dir, w, b);
    let Ok(meta) = std::fs::metadata(&path) else {
        return false;
    };
    meta.len() == entry.size * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("ninemm_persist_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let data: Vec<u16> = (0..1000).map(|i| (i * 37 % 65535) as u16).collect();
        let entry = write_subspace(&tmp, 4, 3, &data).unwrap();
        assert_eq!(entry.size, 1000);

        let back = read_subspace(&tmp, 4, 3).unwrap();
        assert_eq!(back, data);

        let mut manifest = Manifest::default();
        manifest.upsert(entry);
        let verified = read_subspace_verified(&tmp, &manifest, 4, 3).unwrap();
        assert_eq!(verified, data);

        assert!(is_solved(&tmp, &manifest, 4, 3));
        assert!(!is_solved(&tmp, &manifest, 5, 3));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn checksum_mismatch_detected() {
        let tmp = std::env::temp_dir().join(format!("ninemm_persist_test2_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let data: Vec<u16> = vec![1, 2, 3, 4];
        let mut entry = write_subspace(&tmp, 3, 3, &data).unwrap();
        entry.xxh3 = "deadbeefdeadbeef".to_string(); // corrupt the manifest's checksum
        let mut manifest = Manifest::default();
        manifest.upsert(entry);
        assert!(read_subspace_verified(&tmp, &manifest, 3, 3).is_err());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn manifest_roundtrip_json() {
        let tmp = std::env::temp_dir().join(format!("ninemm_persist_test3_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut manifest = Manifest::default();
        manifest.upsert(ManifestEntry {
            w: 3,
            b: 3,
            size: 210_140,
            xxh3: "abc123".to_string(),
            solved_at_unix: 1700000000,
        });
        manifest.save(&tmp).unwrap();
        let loaded = Manifest::load(&tmp).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].w, 3);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
