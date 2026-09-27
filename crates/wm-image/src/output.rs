//! 输出路径规划与输出事务（规格 §8.3、§13.2）。
//!
//! - 默认 NEVER overwrite original：生成 `a_clean.jpg`；保留目录结构时写入独立输出根目录。
//! - 先写临时文件，编码成功后重新打开校验，再原子提交到最终路径。
//! - 失败/取消不留下伪装成成功结果的半成品。

use std::path::{Path, PathBuf};
use wm_core::msg;
use wm_core::settings::{ConflictPolicy, OutputSettings};
use wm_core::{AppError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum PlannedOutput {
    /// 写入该路径。`replace` 为 true 时允许替换已存在文件（仅覆盖原件或 ReplaceOutput 策略）。
    Write { path: PathBuf, replace: bool },
    /// 目标已存在且策略为跳过。
    Skip { existing: PathBuf },
}

/// 规划输出路径。
///
/// * `input` 输入文件；`import_root` 为导入文件夹根（单独导入的文件为 None）；
/// * `ext` 输出扩展名（不含点）。
pub fn plan_output_path(input: &Path, import_root: Option<&Path>, out: &OutputSettings, ext: &str) -> Result<PlannedOutput> {
    let stem = input.file_stem().and_then(|s| s.to_str()).ok_or_else(|| AppError::io(msg!("文件名无效", "Invalid file name")))?;
    let in_ext = input.extension().and_then(|e| e.to_str()).unwrap_or("");

    if out.overwrite_originals {
        // 覆盖原件只在格式不变时允许；格式改变时写到原文件旁边
        if in_ext.eq_ignore_ascii_case(ext) || (is_jpeg_ext(in_ext) && is_jpeg_ext(ext)) {
            return Ok(PlannedOutput::Write { path: input.to_path_buf(), replace: true });
        }
    }

    let dir: PathBuf = match &out.output_dir {
        Some(root) => {
            let mut d = root.clone();
            if out.preserve_structure {
                if let (Some(base), Some(parent)) = (import_root, input.parent()) {
                    if let Ok(rel) = parent.strip_prefix(base) {
                        // 以导入文件夹名作为一级目录，避免多个导入源混在一起
                        if let Some(name) = base.file_name() {
                            d.push(name);
                        }
                        d.push(rel);
                    }
                }
            }
            d
        }
        None => input.parent().map(Path::to_path_buf).unwrap_or_default(),
    };

    let base_name = format!("{stem}{}", out.suffix);
    let candidate = dir.join(format!("{base_name}.{ext}"));
    if same_path(&candidate, input) {
        return Err(AppError::io(msg!(
            "导出路径与原文件相同，已阻止覆盖原件；请设置文件名后缀或导出目录",
            "The output path equals the original, so overwriting was blocked. Set a file name suffix or an output folder"
        )));
    }
    if !candidate.exists() {
        return Ok(PlannedOutput::Write { path: candidate, replace: false });
    }
    match out.conflict {
        ConflictPolicy::Skip => Ok(PlannedOutput::Skip { existing: candidate }),
        ConflictPolicy::ReplaceOutput => Ok(PlannedOutput::Write { path: candidate, replace: true }),
        ConflictPolicy::Number => {
            for n in 2..10_000 {
                let p = dir.join(format!("{base_name} ({n}).{ext}"));
                if !p.exists() && !same_path(&p, input) {
                    return Ok(PlannedOutput::Write { path: p, replace: false });
                }
            }
            Err(AppError::io(msg!("同名导出文件过多", "Too many output files with the same name")))
        }
    }
}

fn is_jpeg_ext(e: &str) -> bool {
    e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg")
}

/// 判断两路径是否指向同一文件（存在时比较规范化路径）。
pub fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// 原子提交：写临时文件 → fsync → `verify` 校验 → 提交。
///
/// `replace = false` 时使用硬链接实现“不覆盖”语义（目标已存在则失败）。
pub fn commit_atomic(dest: &Path, bytes: &[u8], replace: bool, verify: impl FnOnce(&Path) -> Result<()>) -> Result<PathBuf> {
    use std::io::Write;
    let dir = dest.parent().ok_or_else(|| AppError::io(msg!("导出目录无效", "Invalid output folder")))?;
    std::fs::create_dir_all(dir)?;
    let name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("output");
    let tmp = dir.join(format!(".{name}.magies-{}.tmp", wm_common::new_id()));

    let guard = TempGuard(tmp.clone());
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    verify(&tmp)?;

    if replace {
        std::fs::rename(&tmp, dest)?;
    } else {
        match std::fs::hard_link(&tmp, dest) {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(AppError::io(msg!("导出目标已存在，未覆盖", "The output file already exists and was not overwritten")));
            }
            Err(_) => {
                // 部分文件系统（exFAT 等）不支持硬链接：退化为 检查 + rename
                if dest.exists() {
                    return Err(AppError::io(msg!("导出目标已存在，未覆盖", "The output file already exists and was not overwritten")));
                }
                std::fs::rename(&tmp, dest)?;
            }
        }
    }
    std::mem::forget(guard);
    Ok(dest.to_path_buf())
}

/// 失败时删除临时文件。
struct TempGuard(PathBuf);
impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("wmc-out-{}", wm_common::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn default_output_is_sibling_with_suffix() {
        let d = tmpdir();
        let input = d.join("a.jpg");
        std::fs::write(&input, b"x").unwrap();
        let p = plan_output_path(&input, None, &OutputSettings::default(), "jpg").unwrap();
        assert_eq!(p, PlannedOutput::Write { path: d.join("a_clean.jpg"), replace: false });
    }

    #[test]
    fn conflicts_are_numbered() {
        let d = tmpdir();
        let input = d.join("a.jpg");
        std::fs::write(&input, b"x").unwrap();
        std::fs::write(d.join("a_clean.jpg"), b"y").unwrap();
        let p = plan_output_path(&input, None, &OutputSettings::default(), "jpg").unwrap();
        assert_eq!(p, PlannedOutput::Write { path: d.join("a_clean (2).jpg"), replace: false });
    }

    #[test]
    fn preserve_structure_under_output_root() {
        let d = tmpdir();
        let src = d.join("shots");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        let input = src.join("sub/x.png");
        std::fs::write(&input, b"x").unwrap();
        let out = OutputSettings { output_dir: Some(d.join("out")), ..Default::default() };
        let p = plan_output_path(&input, Some(&src), &out, "png").unwrap();
        assert_eq!(p, PlannedOutput::Write { path: d.join("out/shots/sub/x_clean.png"), replace: false });
    }

    #[test]
    fn empty_suffix_same_dir_is_blocked() {
        let d = tmpdir();
        let input = d.join("a.jpg");
        std::fs::write(&input, b"x").unwrap();
        let out = OutputSettings { suffix: String::new(), ..Default::default() };
        assert!(plan_output_path(&input, None, &out, "jpg").is_err());
    }

    #[test]
    fn commit_verifies_and_never_clobbers() {
        let d = tmpdir();
        let dest = d.join("r.bin");
        commit_atomic(&dest, b"hello", false, |_| Ok(())).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        // 第二次不覆盖
        assert!(commit_atomic(&dest, b"again", false, |_| Ok(())).is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello");
        // 校验失败不留下文件
        let dest2 = d.join("bad.bin");
        assert!(commit_atomic(&dest2, b"zz", false, |_| Err(AppError::encode(msg!("坏", "bad")))).is_err());
        assert!(!dest2.exists());
        let leftovers: Vec<_> =
            std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect();
        assert!(leftovers.is_empty());
    }
}
