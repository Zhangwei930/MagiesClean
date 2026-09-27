//! # wm-batch
//!
//! 批量调度（规格 §11）：有界队列、Worker、内存预算、暂停 / 继续 / 取消与进度。
//!
//! - Worker 数默认 CPU 核数 − 1（至少 1，上限 8），GPU（推理）并发默认 1；
//! - 有界 channel：不会同时解码成千上万张图片；
//! - MemoryBudget：按任务估算的解码与中间占用申请预算，超预算时等待，超大单项独占执行；
//! - 暂停：停止领取新任务，正在执行的阶段在安全边界停下；取消：停止后续处理。

use crossbeam_channel::bounded;
use parking_lot::{Condvar, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use wm_core::{AppError, CancellationToken, Result};

/// 暂停闸门。
#[derive(Clone, Default)]
pub struct PauseGate(Arc<(Mutex<bool>, Condvar)>);

impl PauseGate {
    pub fn pause(&self) {
        *self.0 .0.lock() = true;
    }
    pub fn resume(&self) {
        *self.0 .0.lock() = false;
        self.0 .1.notify_all();
    }
    pub fn is_paused(&self) -> bool {
        *self.0 .0.lock()
    }
    /// 暂停时阻塞，直到继续或取消。
    pub fn wait(&self, cancel: &CancellationToken) -> Result<()> {
        let mut g = self.0 .0.lock();
        while *g {
            cancel.check()?;
            self.0 .1.wait_for(&mut g, Duration::from_millis(100));
        }
        cancel.check()
    }
}

/// 批次控制：取消 + 暂停。可从 UI 线程控制。
#[derive(Clone, Default)]
pub struct BatchControl {
    pub cancel: CancellationToken,
    pub pause: PauseGate,
}

impl BatchControl {
    pub fn new() -> Self {
        Self::default()
    }
    /// 在安全边界检查：暂停则等待，取消则返回错误。
    pub fn checkpoint(&self) -> Result<()> {
        self.cancel.check()?;
        self.pause.wait(&self.cancel)
    }
}

/// 内存预算（字节）。
pub struct MemoryBudget {
    limit: u64,
    used: Mutex<u64>,
    cv: Condvar,
    peak: AtomicUsize,
}

pub struct Permit<'a> {
    budget: &'a MemoryBudget,
    bytes: u64,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        let mut u = self.budget.used.lock();
        *u -= self.bytes;
        self.budget.cv.notify_all();
    }
}

impl MemoryBudget {
    pub fn new(limit: u64) -> Self {
        Self { limit: limit.max(1), used: Mutex::new(0), cv: Condvar::new(), peak: AtomicUsize::new(0) }
    }
    pub fn limit(&self) -> u64 {
        self.limit
    }
    /// 申请预算；超过总预算的单项只有在没有其它占用时才放行（独占执行）。
    pub fn acquire(&self, bytes: u64, cancel: &CancellationToken) -> Result<Permit<'_>> {
        let mut u = self.used.lock();
        loop {
            cancel.check()?;
            if *u + bytes <= self.limit || *u == 0 {
                *u += bytes;
                self.peak.fetch_max(*u as usize, Ordering::Relaxed);
                return Ok(Permit { budget: self, bytes });
            }
            self.cv.wait_for(&mut u, Duration::from_millis(100));
        }
    }
    /// 观测到的峰值占用（估算值）。
    pub fn peak(&self) -> u64 {
        self.peak.load(Ordering::Relaxed) as u64
    }
}

#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    pub workers: usize,
    pub memory_budget: u64,
    /// 有界队列容量（默认 workers × 2）。
    pub queue_capacity: usize,
}

impl SchedulerConfig {
    pub fn auto(workers: Option<usize>, memory_budget_mb: Option<u64>) -> Self {
        let cores = num_cpus::get();
        let workers = workers.unwrap_or_else(|| cores.saturating_sub(1).clamp(1, 8)).max(1);
        let memory_budget = memory_budget_mb.map(|m| m * 1024 * 1024).unwrap_or(2 * 1024 * 1024 * 1024);
        Self { workers, memory_budget, queue_capacity: workers * 2 }
    }
}

/// 单项事件。
#[derive(Debug, Clone)]
pub enum ItemEvent<R> {
    Started { index: usize },
    Finished { index: usize, result: std::result::Result<R, AppError> },
}

/// 运行一个批次。`estimate` 返回每项的内存估算（字节）；`work` 执行单项；
/// `on_event` 在 Worker 线程上回调（调用方负责节流与线程安全）。
///
/// 取消后未开始的项以 `Cancelled` 结束；已完成的结果不受影响。
pub fn run<T, R, E, W, F>(
    items: &[T],
    cfg: &SchedulerConfig,
    control: &BatchControl,
    estimate: E,
    work: W,
    on_event: F,
) -> Vec<std::result::Result<R, AppError>>
where
    T: Sync,
    R: Send + Clone,
    E: Fn(&T) -> u64 + Sync,
    W: Fn(usize, &T, &BatchControl) -> Result<R> + Sync,
    F: Fn(ItemEvent<R>) + Sync,
{
    let n = items.len();
    let results: Mutex<Vec<Option<std::result::Result<R, AppError>>>> = Mutex::new((0..n).map(|_| None).collect());
    let budget = MemoryBudget::new(cfg.memory_budget);
    let (tx, rx) = bounded::<usize>(cfg.queue_capacity.max(1));
    std::thread::scope(|s| {
        // 生产者：暂停时停止投递，取消时停止
        let prod_control = control.clone();
        s.spawn(move || {
            for i in 0..n {
                if prod_control.checkpoint().is_err() {
                    break;
                }
                if tx.send(i).is_err() {
                    break;
                }
            }
        });
        for _ in 0..cfg.workers.max(1) {
            let rx = rx.clone();
            let (results, budget, work, estimate, on_event) = (&results, &budget, &work, &estimate, &on_event);
            s.spawn(move || {
                while let Ok(i) = rx.recv() {
                    let r = (|| -> Result<R> {
                        control.checkpoint()?;
                        let _permit = budget.acquire(estimate(&items[i]), &control.cancel)?;
                        on_event(ItemEvent::Started { index: i });
                        work(i, &items[i], control)
                    })();
                    on_event(ItemEvent::Finished { index: i, result: r.clone() });
                    results.lock()[i] = Some(r);
                }
            });
        }
    });
    results.into_inner().into_iter().map(|r| r.unwrap_or_else(|| Err(AppError::cancelled()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn processes_all_items_in_parallel() {
        let items: Vec<u32> = (0..50).collect();
        let cfg = SchedulerConfig { workers: 4, memory_budget: 1 << 30, queue_capacity: 4 };
        let out = run(&items, &cfg, &BatchControl::new(), |_| 1, |_, x, _| Ok(x * 2), |_| {});
        assert_eq!(out.len(), 50);
        assert!(out.iter().enumerate().all(|(i, r)| *r.as_ref().unwrap() == i as u32 * 2));
    }

    #[test]
    fn memory_budget_bounds_concurrency() {
        let items: Vec<u32> = (0..20).collect();
        let cfg = SchedulerConfig { workers: 8, memory_budget: 300, queue_capacity: 8 };
        let live = AtomicU64::new(0);
        let max_live = AtomicU64::new(0);
        let out = run(
            &items,
            &cfg,
            &BatchControl::new(),
            |_| 100,
            |_, _, _| {
                let l = live.fetch_add(1, Ordering::SeqCst) + 1;
                max_live.fetch_max(l, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(5));
                live.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            },
            |_| {},
        );
        assert!(out.iter().all(|r| r.is_ok()));
        assert!(max_live.load(Ordering::SeqCst) <= 3, "max live {}", max_live.load(Ordering::SeqCst));
    }

    #[test]
    fn oversize_item_runs_alone() {
        let b = MemoryBudget::new(100);
        let c = CancellationToken::new();
        let p = b.acquire(500, &c).unwrap();
        assert_eq!(b.peak(), 500);
        drop(p);
        let _a = b.acquire(60, &c).unwrap();
        let _b = b.acquire(40, &c).unwrap();
    }

    #[test]
    fn cancel_stops_remaining_items() {
        let items: Vec<u32> = (0..100).collect();
        let cfg = SchedulerConfig { workers: 2, memory_budget: 1 << 30, queue_capacity: 2 };
        let control = BatchControl::new();
        let done = AtomicU64::new(0);
        let out = run(
            &items,
            &cfg,
            &control,
            |_| 1,
            |i, _, c| {
                if i == 10 {
                    c.cancel.cancel();
                }
                c.checkpoint()?;
                done.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(1));
                Ok(())
            },
            |_| {},
        );
        let ok = out.iter().filter(|r| r.is_ok()).count();
        assert!(ok < 30, "ok={ok}");
        assert!(out.iter().filter(|r| r.is_err()).all(|r| r.as_ref().err().unwrap().kind == wm_core::ErrorKind::Cancelled));
    }

    #[test]
    fn pause_then_resume_completes() {
        let items: Vec<u32> = (0..30).collect();
        let cfg = SchedulerConfig { workers: 2, memory_budget: 1 << 30, queue_capacity: 2 };
        let control = BatchControl::new();
        control.pause.pause();
        let c2 = control.clone();
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            c2.pause.resume();
        });
        let started = std::time::Instant::now();
        let out = run(&items, &cfg, &control, |_| 1, |_, _, _| Ok(()), |_| {});
        h.join().unwrap();
        assert!(started.elapsed() >= Duration::from_millis(140));
        assert!(out.iter().all(|r| r.is_ok()));
    }
}
