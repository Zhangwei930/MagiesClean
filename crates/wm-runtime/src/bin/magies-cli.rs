//! magies-cli：无界面运行完整处理链，用于验证、benchmark 与合成数据集生成。
//!
//! ```text
//! magies-cli process <文件或文件夹>... [--out DIR] [--mode conservative|standard|aggressive] [--quality fast|balanced|best] [--include-review]
//! magies-cli scan <文件或文件夹>... [--json]
//! magies-cli synth --kind batch|single|repeated|negatives --count N --out DIR [--seed S]
//! magies-cli bench <数据集目录> [--json]
//! magies-cli models
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use wm_core::settings::{AutoMode, QualityMode};
use wm_core::tr;
use wm_runtime::dataset::{self, DatasetKind};
use wm_runtime::{Engine, EngineEvent, EventSink};

struct PrintSink;
impl EventSink for PrintSink {
    fn emit(&self, e: EngineEvent) {
        match e {
            EngineEvent::ScanProgress { done, total, phase, .. } => eprintln!("[scan:{phase}] {done}/{total}"),
            EngineEvent::ProcessingProgress { done, total, failed, .. } => eprintln!("[process] {done}/{total} failed={failed}"),
            EngineEvent::Warning { message } => eprintln!("[warn] {message}"),
            _ => {}
        }
    }
}

fn arg_value(args: &[String], key: &str) -> Option<String> {
    args.iter().position(|a| a == key).and_then(|i| args.get(i + 1)).cloned()
}

fn positional(args: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--") {
            skip = !matches!(a.as_str(), "--json" | "--include-review");
            continue;
        }
        out.push(PathBuf::from(a));
    }
    out
}

fn engine() -> Arc<Engine> {
    let data = std::env::var("MAGIES_DATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir().join("magies-cli-data"));
    let models = std::env::var("MAGIES_MODELS").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("models"));
    Engine::new(&data, &models, Arc::new(PrintSink)).unwrap_or_else(|e| fail(&e))
}

/// CLI 语言：环境变量 MAGIES_LANG=zh-CN 使用中文，默认英文。
fn cli_language() -> wm_core::Language {
    let zh = std::env::var("MAGIES_LANG").map(|v| v.to_lowercase().starts_with("zh")).unwrap_or(false);
    if zh {
        wm_core::Language::ZhCn
    } else {
        wm_core::Language::En
    }
}

fn fail(e: &wm_core::AppError) -> ! {
    eprintln!("{} {}: {} ({})", tr!("错误", "Error"), e.code(), e.message, e.next_step());
    std::process::exit(1)
}

fn main() {
    wm_runtime::init_logging(None);
    // CLI 语言：MAGIES_LANG=zh-CN 使用中文，默认英文
    wm_core::i18n::set_language(cli_language());
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else {
        eprintln!("{}: magies-cli <process|scan|synth|bench|models> ...", tr!("用法", "Usage"));
        std::process::exit(2);
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "process" | "scan" => {
            let e = engine();
            let mut s = e.settings();
            s.language = cli_language();
            if let Some(m) = arg_value(rest, "--mode") {
                s.auto_mode = match m.as_str() {
                    "conservative" => AutoMode::Conservative,
                    "aggressive" => AutoMode::Aggressive,
                    _ => AutoMode::Standard,
                };
            }
            if let Some(q) = arg_value(rest, "--quality") {
                s.quality_mode = match q.as_str() {
                    "fast" => QualityMode::Fast,
                    "best" => QualityMode::Best,
                    _ => QualityMode::Balanced,
                };
            }
            if let Some(o) = arg_value(rest, "--out") {
                s.output.output_dir = Some(PathBuf::from(o));
            }
            e.update_settings(s).unwrap_or_else(|x| fail(&x));
            let r = e.import(positional(rest)).unwrap_or_else(|x| fail(&x));
            eprintln!(
                "{}",
                tr!(
                    format!("导入 {} 个文件（重复 {}，不支持 {}）", r.added.len(), r.duplicates, r.unsupported.len()),
                    format!("Imported {} file(s) ({} duplicate, {} unsupported)", r.added.len(), r.duplicates, r.unsupported.len())
                )
            );
            e.scan_blocking(None).unwrap_or_else(|x| fail(&x));
            if cmd == "scan" {
                let files = e.files();
                if rest.iter().any(|a| a == "--json") {
                    println!("{}", serde_json::to_string_pretty(&files).unwrap());
                } else {
                    for f in files {
                        println!(
                            "{:<40} {:?} {} {} {} {} {} {}",
                            f.name,
                            f.status,
                            tr!("候选", "detections"),
                            f.candidates.len(),
                            tr!("自动", "auto"),
                            f.summary.to_remove,
                            tr!("复核", "review"),
                            f.summary.needs_review
                        );
                        for c in &f.candidates {
                            println!(
                                "    {:<10} {:>5.1}%  {:?}  [{}]",
                                c.type_label,
                                c.confidence * 100.0,
                                c.decision,
                                c.sources.join(", ")
                            );
                        }
                        for n in &f.notes {
                            println!("    · {n}");
                        }
                    }
                }
                return;
            }
            if rest.iter().any(|a| a == "--include-review") {
                for f in e.files() {
                    let _ = e.resolve_pending(&f.id, wm_core::UserAction::Remove);
                }
            }
            let sum = e.process_blocking(None).unwrap_or_else(|x| fail(&x));
            for f in e.files() {
                let q = f.quality.as_ref().map(|q| format!("{:.2}", q.score)).unwrap_or("-".into());
                let route = f.route.as_ref().map(|r| r.route.label()).unwrap_or("-");
                println!(
                    "{:<40} {:?} {}={route} {}={q} {}={}",
                    f.name,
                    f.status,
                    tr!("路由", "route"),
                    tr!("质量", "quality"),
                    tr!("输出", "output"),
                    f.output.as_deref().unwrap_or("-")
                );
                if let Some(err) = &f.error {
                    println!("    ! {}: {}", err.message, err.next_step);
                }
            }
            println!(
                "{}",
                tr!(
                    format!(
                        "完成 {} / 失败 {} / 需复核 {} / 跳过 {}，耗时 {} ms",
                        sum.completed, sum.failed, sum.needs_review, sum.skipped, sum.elapsed_ms
                    ),
                    format!(
                        "Completed {} / failed {} / needs review {} / skipped {} in {} ms",
                        sum.completed, sum.failed, sum.needs_review, sum.skipped, sum.elapsed_ms
                    )
                )
            );
        }
        "synth" => {
            let out = PathBuf::from(arg_value(rest, "--out").unwrap_or_else(|| "dataset".into()));
            let count: usize = arg_value(rest, "--count").and_then(|c| c.parse().ok()).unwrap_or(20);
            let seed: u64 = arg_value(rest, "--seed").and_then(|c| c.parse().ok()).unwrap_or(7);
            let kind = match arg_value(rest, "--kind").as_deref() {
                Some("single") => DatasetKind::Single,
                Some("repeated") => DatasetKind::Repeated,
                Some("negatives") => DatasetKind::Negatives,
                _ => DatasetKind::Batch,
            };
            let m = dataset::generate(&out, kind, count, seed).unwrap_or_else(|x| fail(&x));
            println!(
                "{}",
                tr!(
                    format!("已生成 {} 个样本到 {}", m.len(), out.display()),
                    format!("Generated {} samples in {}", m.len(), out.display())
                )
            );
        }
        "bench" => {
            let dir = positional(rest).into_iter().next().unwrap_or_else(|| PathBuf::from("dataset"));
            let e = engine();
            let r = dataset::bench(&e, &dir).unwrap_or_else(|x| fail(&x));
            if rest.iter().any(|a| a == "--json") {
                println!("{}", serde_json::to_string_pretty(&r).unwrap());
            } else {
                println!(
                    "{} {} ({} {}, {} {})",
                    tr!("样本", "Samples"),
                    r.samples,
                    tr!("正样本", "positives"),
                    r.positives,
                    tr!("负样本", "negatives"),
                    r.negatives
                );
                println!(
                    "{} Precision {:.1}%  Recall {:.1}%",
                    tr!("自动决策", "Auto decisions"),
                    r.auto_precision * 100.0,
                    r.auto_recall * 100.0
                );
                println!("{} {:.1}%", tr!("候选 Recall（自动 + 复核）", "Candidate recall (auto + review)"), r.candidate_recall * 100.0);
                println!(
                    "{} {:.1}%",
                    tr!("负样本自动误判率", "Negative auto false-positive rate"),
                    r.negative_auto_false_positive_rate * 100.0
                );
                println!("{} {:.3}", tr!("平均 Mask IoU", "Mean mask IoU"), r.mean_mask_iou);
                println!("{} {} ms, {} {} ms", tr!("总耗时", "Total"), r.total_ms, tr!("平均每文件", "avg per file"), r.scan_ms_avg);
                for f in r.failures.iter().take(20) {
                    println!("  - {f}");
                }
            }
        }
        "models" => {
            let e = engine();
            for s in e.model_statuses() {
                println!("{:<14} {:<10} {:<8} {}", s.id, s.role.label(), s.version, s.label());
            }
        }
        _ => {
            eprintln!("{}: {cmd}", tr!("未知命令", "Unknown command"));
            std::process::exit(2);
        }
    }
}
