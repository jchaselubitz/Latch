//! SHA-256 over a file, for verifying a downloaded release archive.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Hex SHA-256 of a file, read in bounded chunks so a large archive never has
/// to be resident.
pub fn hex_of_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(crate::hex::encode(&hasher.finalize()));
        }
        hasher.update(&buffer[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_a_file_the_way_it_hashes_the_same_bytes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("archive.bin");
        let bytes: Vec<u8> = (0..200_000u32).map(|index| index as u8).collect();
        std::fs::write(&path, &bytes).expect("write");
        assert_eq!(
            hex_of_file(&path).expect("hash file"),
            crate::hex::encode(&Sha256::digest(&bytes))
        );
    }

    #[test]
    fn matches_the_fips_180_4_vector_for_abc() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("abc");
        std::fs::write(&path, b"abc").expect("write");
        assert_eq!(
            hex_of_file(&path).expect("hash file"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
