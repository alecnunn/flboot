use std::io::Read;
use std::path::Path;

pub fn sha1_file(path: &Path) -> anyhow::Result<String> {
    use sha1::{Digest, Sha1};
    let mut file = std::fs::File::open(path).map_err(|e| anyhow::anyhow!("opening {}: {}", path.display(), e))?;
    let mut hasher = Sha1::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

pub fn verify_hash(required: bool, path: &Path, expected: &str) -> anyhow::Result<()> {
    let expected = expected.to_lowercase();
    if !path.exists() {
        let msg = format!("missing required file: {}", path.display());
        if required {
            anyhow::bail!(msg);
        }
        crate::log::warn(&msg);
        return Ok(());
    }
    let actual = sha1_file(path)?;
    if actual.to_lowercase() != expected {
        let msg = format!("{} sha1 invalid expected:{} actual:{}", path.display(), expected, actual);
        if required {
            anyhow::bail!(msg);
        }
        crate::log::warn(&msg);
        return Ok(());
    }
    crate::log::info(&format!("{} sha1 verified", path.display()));
    Ok(())
}

pub fn download_file(url: &str, dest: &Path, expected_sha1: Option<&str>) -> anyhow::Result<()> {
    if dest.exists() {
        let matches = match expected_sha1 {
            Some(expected) => sha1_file(dest)?.eq_ignore_ascii_case(expected),
            None => true,
        };
        if matches {
            crate::log::info(&format!(
                "{} already present{}",
                dest.display(),
                if expected_sha1.is_some() { " and verified" } else { "" }
            ));
            return Ok(());
        }
        crate::log::warn(&format!("sha1 mismatch for {}, re-acquiring", dest.display()));
        std::fs::remove_file(dest)?;
    }

    crate::log::info(&format!("downloading {url} -> {}", dest.display()));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let response = ureq::get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36",
        )
        .call()
        .map_err(|e| anyhow::anyhow!("downloading {url}: {e}"))?;

    let mut reader = response.into_body().into_reader();
    let mut file = std::fs::File::create(dest)?;
    std::io::copy(&mut reader, &mut file)?;
    drop(file);

    if let Some(expected) = expected_sha1 {
        let actual = sha1_file(dest)?;
        if !actual.eq_ignore_ascii_case(expected) {
            anyhow::bail!("sha1 mismatch for {}: expected {}, got {}", dest.display(), expected, actual);
        }
        crate::log::info(&format!("{} downloaded and verified", dest.display()));
    } else {
        crate::log::info(&format!("{} downloaded", dest.display()));
    }
    Ok(())
}

pub fn extract_zip_entry(zip_path: &Path, entry_name: &str, dest_dir: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut entry = archive.by_name(entry_name)?;
    std::fs::create_dir_all(dest_dir)?;
    let mut out = std::fs::File::create(dest_dir.join(entry_name))?;
    std::io::copy(&mut entry, &mut out)?;
    Ok(())
}

pub fn extract_tar_gz(archive_path: &Path, dest_dir: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    std::fs::create_dir_all(dest_dir)?;
    archive.unpack(dest_dir)?;
    Ok(())
}

// Only the unix seven-zip download path calls this; keep it compiled and
// tested everywhere, but silence dead-code on Windows where nothing calls it.
#[cfg_attr(windows, allow(dead_code))]
pub fn extract_tar_xz(archive_path: &Path, dest_dir: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(archive_path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut decompressed = Vec::new();
    lzma_rs::xz_decompress(&mut reader, &mut decompressed)
        .map_err(|e| anyhow::anyhow!("xz decompress of {}: {e:?}", archive_path.display()))?;
    let mut archive = tar::Archive::new(std::io::Cursor::new(decompressed));
    std::fs::create_dir_all(dest_dir)?;
    archive.unpack(dest_dir)?;
    Ok(())
}

#[cfg(unix)]
pub fn set_executable(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(windows)]
pub fn set_executable(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("flboot-fetch-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sha1_file_matches_known_digest() {
        let dir = temp_dir("sha1");
        let path = dir.join("known.txt");
        std::fs::write(&path, b"abc").unwrap();
        let digest = sha1_file(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(digest, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn verify_hash_ok_on_match() {
        let dir = temp_dir("verify-ok");
        let path = dir.join("known.txt");
        std::fs::write(&path, b"abc").unwrap();
        let result = verify_hash(true, &path, "a9993e364706816aba3e25717850c26c9cd0d89d");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn verify_hash_errors_on_mismatch_when_required() {
        let dir = temp_dir("verify-mismatch");
        let path = dir.join("known.txt");
        std::fs::write(&path, b"abc").unwrap();
        let result = verify_hash(true, &path, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn verify_hash_warns_without_erroring_when_optional() {
        let dir = temp_dir("verify-optional");
        let path = dir.join("missing.txt");
        let result = verify_hash(false, &path, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
        std::fs::remove_dir_all(&dir).ok();
        assert!(result.is_ok());
    }

    #[test]
    fn extract_zip_entry_roundtrips() {
        let dir = temp_dir("zip");
        let zip_path = dir.join("test.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options: zip::write::FileOptions<()> = zip::write::FileOptions::default();
            zip.start_file("ninja.exe", options).unwrap();
            zip.write_all(b"fake ninja binary").unwrap();
            zip.finish().unwrap();
        }
        extract_zip_entry(&zip_path, "ninja.exe", &dir).unwrap();
        let extracted = std::fs::read_to_string(dir.join("ninja.exe")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(extracted, "fake ninja binary");
    }

    #[test]
    fn extract_tar_gz_roundtrips() {
        let dir = temp_dir("targz");
        let archive_path = dir.join("test.tar.gz");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(5);
            header.set_cksum();
            builder.append_data(&mut header, "Bin/marker.txt", &b"hello"[..]).unwrap();
            builder.finish().unwrap();
        }
        let extract_dir = dir.join("extracted");
        extract_tar_gz(&archive_path, &extract_dir).unwrap();
        let extracted = std::fs::read_to_string(extract_dir.join("Bin/marker.txt")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(extracted, "hello");
    }

    #[test]
    fn extract_tar_xz_roundtrips() {
        let dir = temp_dir("tarxz");
        let archive_path = dir.join("test.tar.xz");
        {
            let mut tar_bytes = Vec::new();
            {
                let mut builder = tar::Builder::new(&mut tar_bytes);
                let mut header = tar::Header::new_gnu();
                header.set_size(5);
                header.set_cksum();
                builder.append_data(&mut header, "7zzs", &b"hello"[..]).unwrap();
                builder.finish().unwrap();
            }
            let xz_bytes = {
                let mut compressed = Vec::new();
                lzma_rs::xz_compress(&mut std::io::Cursor::new(&tar_bytes), &mut compressed).unwrap();
                compressed
            };
            std::fs::write(&archive_path, xz_bytes).unwrap();
        }
        let extract_dir = dir.join("extracted");
        extract_tar_xz(&archive_path, &extract_dir).unwrap();
        let extracted = std::fs::read_to_string(extract_dir.join("7zzs")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(extracted, "hello");
    }

    #[cfg(unix)]
    #[test]
    fn set_executable_adds_execute_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("chmod");
        let path = dir.join("tool");
        std::fs::write(&path, b"binary").unwrap();
        let before = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(before & 0o111, 0);

        set_executable(&path).unwrap();

        let after = std::fs::metadata(&path).unwrap().permissions().mode();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(after & 0o111, 0o111);
    }
}
