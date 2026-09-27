//! # wm-common
//!
//! 通用 ID、文件指纹与少量基础工具。只放无业务语义的工具，避免成为跨层业务逻辑集合。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 新的随机 ID（UUID v4，无连字符以便作为缓存文件名）。
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 当前 Unix 毫秒时间戳。
pub fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// 快速指纹：文件大小 + 修改时间 + 首尾各 64 KiB 的 BLAKE3。
/// 用于快速判断输入是否改变；需要强一致性时使用 [`sha256_file`]。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FastFingerprint(pub String);

const FP_CHUNK: u64 = 64 * 1024;

pub fn fast_fingerprint(path: &Path) -> std::io::Result<FastFingerprint> {
    let meta = std::fs::metadata(path)?;
    let len = meta.len();
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&len.to_le_bytes());
    hasher.update(&mtime.to_le_bytes());
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; FP_CHUNK.min(len) as usize];
    f.read_exact(&mut buf)?;
    hasher.update(&buf);
    if len > FP_CHUNK * 2 {
        f.seek(SeekFrom::End(-(FP_CHUNK as i64)))?;
        f.read_exact(&mut buf)?;
        hasher.update(&buf);
    }
    Ok(FastFingerprint(hasher.finalize().to_hex()[..32].to_string()))
}

/// 完整内容 SHA-256（十六进制小写）。
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 对任意可序列化配置求稳定哈希（用于缓存身份）。
pub fn hash_str(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex()[..16].to_string()
}

/// 支持的图片扩展名（小写）。
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff"];
pub const PDF_EXTENSIONS: &[&str] = &["pdf"];

pub fn lower_ext(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase())
}

pub fn is_supported_image(path: &Path) -> bool {
    lower_ext(path).is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
}

pub fn is_pdf(path: &Path) -> bool {
    lower_ext(path).is_some_and(|e| PDF_EXTENSIONS.contains(&e.as_str()))
}

/// 人类可读的文件大小。
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn fingerprint_changes_with_content() {
        let dir = std::env::temp_dir().join(format!("wmc-fp-{}", new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.bin");
        std::fs::write(&p, vec![1u8; 200_000]).unwrap();
        let a = fast_fingerprint(&p).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&[2u8; 10]).unwrap();
        drop(f);
        let b = fast_fingerprint(&p).unwrap();
        assert_ne!(a, b);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ext_checks() {
        assert!(is_supported_image(Path::new("/a/B.JPG")));
        assert!(is_supported_image(Path::new("x.tiff")));
        assert!(!is_supported_image(Path::new("x.heic")));
        assert!(is_pdf(Path::new("doc.PDF")));
        assert_eq!(human_size(1536), "1.5 KB");
    }
}
