//! Persistent, on-disk cache of shallow opening-search transposition-table
//! entries (design-opening-phase.md, implementation-opening-phase.md).
//! One self-describing file (`opening_cache.bin`), written once by
//! `build-opening-cache` and loaded read-only by every opening-search
//! consumer to pre-populate its in-memory `Tt` (see `opening.rs`).
//!
//! File format (all integers native-endian, matching `persist.rs`'s
//! `as_bytes` convention):
//!
//! ```text
//! [8 bytes]  magic + version: b"NMMOPEN1"
//! [8 bytes]  database fingerprint (u64), see `db_fingerprint`
//! [8 bytes]  entry count n (u64)
//! [8 bytes]  payload checksum: xxh3_64 of the n*9 entry bytes that follow
//! [n * 9]    entries, sorted ascending by packed key:
//!            [8 bytes packed key][1 byte packed value+bound]
//! ```
//!
//! Packed key: `pos.0` (white in bits 0..24, black in bits 24..48, per
//! `pos.rs`) with `mover_hand` in bits 48..52 and `opp_hand` in bits
//! 52..56; bits 56..64 are reserved and must be zero.
//!
//! Packed value: exactly five legal bytes, `(value + 1) | (bound_code <<
//! 2)` with `Exact = 0`, `Lower = 1`, `Upper = 2` — see `pack_value`/
//! `unpack_value`. Only `Draw` (`0`) is ever stored as a bound (see
//! `opening::negamax`'s bound classification), hence exactly five values
//! (`0, 1, 2, 5, 9`) are legal; anything else means the file is corrupt.

use crate::opening::{Bound, Tt};
use crate::persist::Manifest;
use crate::pos::Position;
use std::path::Path;

pub const CACHE_FILENAME: &str = "opening_cache.bin";

const MAGIC: &[u8; 8] = b"NMMOPEN1";
const HEADER_LEN: usize = 32;
const ENTRY_LEN: usize = 9;

fn pack_key(pos: Position, mover_hand: u8, opp_hand: u8) -> u64 {
    debug_assert!(mover_hand <= 9 && opp_hand <= 9);
    pos.0 | ((mover_hand as u64) << 48) | ((opp_hand as u64) << 52)
}

/// `None` iff the key is structurally invalid (reserved bits set, hands
/// out of range, or overlapping white/black) — treat as file corruption.
fn unpack_key(key: u64) -> Option<(Position, u8, u8)> {
    if key >> 56 != 0 {
        return None; // spare bits must be zero in version 1
    }
    let white = (key & 0xFF_FFFF) as u32;
    let black = ((key >> 24) & 0xFF_FFFF) as u32;
    let mover_hand = ((key >> 48) & 0xF) as u8;
    let opp_hand = ((key >> 52) & 0xF) as u8;
    if white & black != 0 || mover_hand > 9 || opp_hand > 9 {
        return None;
    }
    Some((Position::new(white, black), mover_hand, opp_hand))
}

fn pack_value(value: i8, bound: Bound) -> u8 {
    let bound_code: u8 = match bound {
        Bound::Exact => 0,
        Bound::Lower => 1,
        Bound::Upper => 2,
    };
    ((value + 1) as u8) | (bound_code << 2)
}

/// `None` iff the byte is not one of the five legal encodings.
fn unpack_value(byte: u8) -> Option<(i8, Bound)> {
    match byte {
        0 => Some((-1, Bound::Exact)),
        1 => Some((0, Bound::Exact)),
        2 => Some((1, Bound::Exact)),
        5 => Some((0, Bound::Lower)),
        9 => Some((0, Bound::Upper)),
        _ => None,
    }
}

/// A fingerprint over the *current* movement-phase database's manifest:
/// content-based (keyed by each subspace's stored xxh3, not by when it
/// was solved), so re-solving a pair to identical bytes does not spuriously
/// invalidate a cache built against it, but any actual content change
/// does. Used to detect a stale cache (see `load_or_empty`).
pub fn db_fingerprint(manifest: &Manifest) -> u64 {
    let mut entries: Vec<&crate::persist::ManifestEntry> = manifest.entries.iter().collect();
    entries.sort_by_key(|e| (e.w, e.b));
    let mut buf = String::new();
    for e in entries {
        buf.push_str(&format!("{}-{}:{};", e.w, e.b, e.xxh3));
    }
    xxhash_rust::xxh3::xxh3_64(buf.as_bytes())
}

/// Filter `tt` to entries with `mover_hand + opp_hand >= min_hand_sum`
/// (a ply-`p` state has `mover_hand + opp_hand == 18 - p`, so this keeps
/// states within `18 - min_hand_sum` plies of the empty board), pack,
/// sort ascending by key, and write atomically: build the full byte
/// buffer in memory (this file is a few MB at most, unlike the
/// multi-GB movement-phase files), write to a `.tmp` path, then
/// `rename` into place, mirroring `persist::write_subspace`. Returns
/// `(entries_written, file_bytes)`.
pub fn write_cache(dir: &Path, fingerprint: u64, tt: &Tt, min_hand_sum: u8) -> anyhow::Result<(usize, u64)> {
    let mut entries: Vec<(u64, u8)> = tt
        .iter()
        .filter(|((_, mover_hand, opp_hand), _)| mover_hand + opp_hand >= min_hand_sum)
        .map(|(&(pos, mover_hand, opp_hand), &(value, bound))| {
            (pack_key(pos, mover_hand, opp_hand), pack_value(value, bound))
        })
        .collect();
    entries.sort_unstable_by_key(|&(key, _)| key);

    let mut payload = Vec::with_capacity(entries.len() * ENTRY_LEN);
    for (key, value) in &entries {
        payload.extend_from_slice(&key.to_ne_bytes());
        payload.push(*value);
    }
    let payload_checksum = xxhash_rust::xxh3::xxh3_64(&payload);

    let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&fingerprint.to_ne_bytes());
    buf.extend_from_slice(&(entries.len() as u64).to_ne_bytes());
    buf.extend_from_slice(&payload_checksum.to_ne_bytes());
    buf.extend_from_slice(&payload);

    std::fs::create_dir_all(dir)?;
    let path = dir.join(CACHE_FILENAME);
    let tmp_path = path.with_extension("bin.tmp");
    {
        use std::io::Write;
        let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp_path)?);
        f.write_all(&buf)?;
        f.flush()?;
    }
    std::fs::rename(&tmp_path, &path)?; // atomic on the same filesystem

    Ok((entries.len(), buf.len() as u64))
}

enum ParseOutcome {
    Ok(Tt),
    Stale,
    Corrupt(&'static str),
}

fn parse_cache(bytes: &[u8], manifest: &Manifest) -> ParseOutcome {
    if bytes.len() < HEADER_LEN {
        return ParseOutcome::Corrupt("file too short for header");
    }
    if &bytes[0..8] != MAGIC {
        return ParseOutcome::Corrupt("bad magic/version");
    }
    let fingerprint = u64::from_ne_bytes(bytes[8..16].try_into().unwrap());
    if fingerprint != db_fingerprint(manifest) {
        return ParseOutcome::Stale;
    }
    let n = u64::from_ne_bytes(bytes[16..24].try_into().unwrap());
    let Some(payload_len) = (n as usize).checked_mul(ENTRY_LEN) else {
        return ParseOutcome::Corrupt("entry count overflows payload length");
    };
    let Some(expected_len) = HEADER_LEN.checked_add(payload_len) else {
        return ParseOutcome::Corrupt("entry count overflows file length");
    };
    if bytes.len() != expected_len {
        return ParseOutcome::Corrupt("file length does not match header entry count");
    }
    let stored_checksum = u64::from_ne_bytes(bytes[24..32].try_into().unwrap());
    let payload = &bytes[HEADER_LEN..];
    if xxhash_rust::xxh3::xxh3_64(payload) != stored_checksum {
        return ParseOutcome::Corrupt("payload checksum mismatch");
    }

    let mut tt = Tt::new();
    let mut prev_key: Option<u64> = None;
    for chunk in payload.chunks_exact(ENTRY_LEN) {
        let key = u64::from_ne_bytes(chunk[0..8].try_into().unwrap());
        if prev_key.is_some_and(|pk| key <= pk) {
            return ParseOutcome::Corrupt("entries not strictly ascending by key");
        }
        prev_key = Some(key);
        let Some((pos, mover_hand, opp_hand)) = unpack_key(key) else {
            return ParseOutcome::Corrupt("invalid packed key");
        };
        let Some((value, bound)) = unpack_value(chunk[8]) else {
            return ParseOutcome::Corrupt("invalid packed value");
        };
        tt.insert((pos, mover_hand, opp_hand), (value, bound));
    }
    ParseOutcome::Ok(tt)
}

/// Load the on-disk opening cache into a fresh `Tt`, or an empty one if
/// it's absent, stale, or corrupt in any way. Every failure path is
/// silent-or-logged-and-empty, never a panic or an `Err`: a bad cache
/// file must degrade a search to "no cache", not break the command that
/// needed it. At the expected size of a few MB, loading every entry into
/// the `HashMap` up front is simpler than lazy access and fine.
pub fn load_or_empty(dir: &Path, manifest: &Manifest) -> Tt {
    let path = dir.join(CACHE_FILENAME);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Tt::new(),
        Err(e) => {
            eprintln!("opening cache at {} could not be read ({e}); ignoring", path.display());
            return Tt::new();
        }
    };
    match parse_cache(&bytes, manifest) {
        ParseOutcome::Ok(tt) => {
            eprintln!("loaded {} opening-cache entries from {}", tt.len(), path.display());
            tt
        }
        ParseOutcome::Stale => {
            eprintln!(
                "opening cache at {} is stale (database has changed since it was built); ignoring",
                path.display()
            );
            Tt::new()
        }
        ParseOutcome::Corrupt(reason) => {
            eprintln!("opening cache at {} is corrupt ({reason}); ignoring", path.display());
            Tt::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persist::ManifestEntry;

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ninemm_opcache_test_{tag}_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn manifest_with(entries: &[(u8, u8, &str)]) -> Manifest {
        let mut m = Manifest::default();
        for &(w, b, xxh3) in entries {
            m.upsert(ManifestEntry { w, b, size: 1, xxh3: xxh3.to_string(), solved_at_unix: 0 });
        }
        m
    }

    #[test]
    fn round_trip() {
        let dir = tmp_dir("roundtrip");
        let manifest = manifest_with(&[(3, 3, "aaaa"), (3, 4, "bbbb")]);
        let fp = db_fingerprint(&manifest);

        let mut tt = Tt::new();
        tt.insert((Position::new(1, 2), 3, 4), (-1, Bound::Exact));
        tt.insert((Position::new(4, 8), 5, 2), (0, Bound::Exact));
        tt.insert((Position::new(16, 32), 9, 9), (1, Bound::Exact));
        tt.insert((Position::new(64, 128), 0, 1), (0, Bound::Lower));
        tt.insert((Position::new(256, 512), 2, 0), (0, Bound::Upper));

        let (written, size) = write_cache(&dir, fp, &tt, 0).unwrap();
        assert_eq!(written, 5);
        assert!(size > 0);

        let loaded = load_or_empty(&dir, &manifest);
        assert_eq!(loaded, tt);

        // Raw bytes are sorted ascending by key.
        let bytes = std::fs::read(dir.join(CACHE_FILENAME)).unwrap();
        let payload = &bytes[HEADER_LEN..];
        let mut prev: Option<u64> = None;
        for chunk in payload.chunks_exact(ENTRY_LEN) {
            let key = u64::from_ne_bytes(chunk[0..8].try_into().unwrap());
            if let Some(p) = prev {
                assert!(key > p, "entries must be strictly ascending");
            }
            prev = Some(key);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn threshold_filter() {
        let dir = tmp_dir("threshold");
        let manifest = manifest_with(&[(3, 3, "aaaa")]);
        let fp = db_fingerprint(&manifest);

        let mut tt = Tt::new();
        tt.insert((Position::new(1, 2), 1, 1), (0, Bound::Exact)); // sum 2, below
        tt.insert((Position::new(4, 8), 5, 5), (0, Bound::Exact)); // sum 10, at/above

        write_cache(&dir, fp, &tt, 10).unwrap();
        let loaded = load_or_empty(&dir, &manifest);
        assert_eq!(loaded.len(), 1);
        assert!(loaded.contains_key(&(Position::new(4, 8), 5, 5)));
        assert!(!loaded.contains_key(&(Position::new(1, 2), 1, 1)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tmp_dir("missing");
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = manifest_with(&[(3, 3, "aaaa")]);
        let loaded = load_or_empty(&dir, &manifest);
        assert!(loaded.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupted_payload_is_rejected() {
        let dir = tmp_dir("corruptpayload");
        let manifest = manifest_with(&[(3, 3, "aaaa")]);
        let fp = db_fingerprint(&manifest);

        let mut tt = Tt::new();
        tt.insert((Position::new(1, 2), 1, 1), (0, Bound::Exact));
        write_cache(&dir, fp, &tt, 0).unwrap();

        let path = dir.join(CACHE_FILENAME);
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF; // flip a byte inside the single entry
        std::fs::write(&path, &bytes).unwrap();

        let loaded = load_or_empty(&dir, &manifest);
        assert!(loaded.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_value_byte_with_correct_checksum_is_rejected() {
        // Hand-build a file whose payload checksum is internally
        // consistent but whose single value byte is not one of the five
        // legal encodings, to exercise the decoder path specifically
        // (independent of the checksum check in the previous test).
        let dir = tmp_dir("invalidvalue");
        let manifest = manifest_with(&[(3, 3, "aaaa")]);
        let fp = db_fingerprint(&manifest);

        let key = pack_key(Position::new(1, 2), 1, 1);
        let mut payload = Vec::new();
        payload.extend_from_slice(&key.to_ne_bytes());
        payload.push(7); // not a legal packed value
        let checksum = xxhash_rust::xxh3::xxh3_64(&payload);

        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&fp.to_ne_bytes());
        buf.extend_from_slice(&1u64.to_ne_bytes());
        buf.extend_from_slice(&checksum.to_ne_bytes());
        buf.extend_from_slice(&payload);

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CACHE_FILENAME), &buf).unwrap();

        let loaded = load_or_empty(&dir, &manifest);
        assert!(loaded.is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn truncated_and_bad_magic_are_rejected() {
        let dir = tmp_dir("truncated");
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = manifest_with(&[(3, 3, "aaaa")]);

        std::fs::write(dir.join(CACHE_FILENAME), b"short").unwrap();
        assert!(load_or_empty(&dir, &manifest).is_empty());

        let mut bad_magic = vec![0u8; HEADER_LEN];
        bad_magic[0..8].copy_from_slice(b"NOTAMAGC");
        std::fs::write(dir.join(CACHE_FILENAME), &bad_magic).unwrap();
        assert!(load_or_empty(&dir, &manifest).is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn staleness_is_detected() {
        let dir = tmp_dir("staleness");
        let manifest_a = manifest_with(&[(3, 3, "aaaa")]);
        let manifest_b = manifest_with(&[(3, 3, "bbbb")]); // different checksum content
        assert_ne!(db_fingerprint(&manifest_a), db_fingerprint(&manifest_b));

        let mut tt = Tt::new();
        tt.insert((Position::new(1, 2), 1, 1), (0, Bound::Exact));
        write_cache(&dir, db_fingerprint(&manifest_a), &tt, 0).unwrap();

        assert!(!load_or_empty(&dir, &manifest_a).is_empty());
        assert!(load_or_empty(&dir, &manifest_b).is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let a = manifest_with(&[(3, 3, "aaaa"), (3, 4, "bbbb"), (4, 3, "cccc")]);
        let b = manifest_with(&[(4, 3, "cccc"), (3, 3, "aaaa"), (3, 4, "bbbb")]);
        assert_eq!(db_fingerprint(&a), db_fingerprint(&b));
    }
}
