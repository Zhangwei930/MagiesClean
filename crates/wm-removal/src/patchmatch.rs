//! 多尺度样本块合成（Wexler 等 “Space-Time Completion” + PatchMatch 最近邻场）。
//!
//! 适用于纹理较复杂但没有 AI 模型可用的情形：用图像中已知区域的真实纹理块
//! 重建洞区，而不是像扩散类方法那样模糊插值。确定性（固定种子），便于 Golden 测试。

use crate::telea;
use wm_core::{AppError, CancellationToken, Result};

#[derive(Debug, Clone, Copy)]
pub struct SynthParams {
    /// 块大小（奇数）。
    pub patch: usize,
    pub iters_coarse: usize,
    pub iters_fine: usize,
    pub seed: u64,
}

impl Default for SynthParams {
    fn default() -> Self {
        Self { patch: 7, iters_coarse: 8, iters_fine: 3, seed: 0x9E37_79B9_7F4A_7C15 }
    }
}

type Px = [f32; 3];

struct Level {
    w: usize,
    h: usize,
    img: Vec<Px>,
    hole: Vec<bool>,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }
}

/// 修复 RGB 平面。`planes` 为 3 个 w×h 平面，原地写回洞区像素。
pub fn synthesize(
    planes: &mut [Vec<f32>],
    w: usize,
    h: usize,
    hole: &[bool],
    params: SynthParams,
    cancel: &CancellationToken,
) -> Result<()> {
    if !hole.iter().any(|&b| b) {
        return Ok(());
    }
    let half = params.patch / 2;
    let img: Vec<Px> = (0..w * h).map(|i| [planes[0][i], planes[1][i], planes[2][i]]).collect();
    let mut levels = vec![Level { w, h, img, hole: hole.to_vec() }];

    // 构建金字塔：直到洞足够小或图像太小
    loop {
        let l = levels.last().unwrap();
        let (hw, hh) = hole_extent(l);
        if hw.max(hh) <= params.patch * 2 || l.w.min(l.h) < params.patch * 4 || levels.len() >= 7 {
            break;
        }
        let next = downsample(l);
        if count_valid_sources(&next, half) < 16 {
            break;
        }
        levels.push(next);
    }

    let mut rng = Rng(params.seed | 1);
    let coarsest = levels.len() - 1;

    for li in (0..=coarsest).rev() {
        cancel.check()?;
        if li == coarsest {
            init_with_telea(&mut levels[li]);
        } else {
            // 从粗层上采样洞区像素
            let (coarse, fine) = split_pair(&mut levels, li);
            upsample_hole_pixels(coarse, fine);
        }
        let lv = &levels[li];
        let valid = valid_sources(lv, half);
        if !valid.iter().any(|&v| v) {
            // 没有完整的已知块可用：保持 Telea 结果
            if li == 0 {
                let mut p: Vec<Vec<f32>> = (0..3).map(|c| lv.img.iter().map(|px| px[c]).collect()).collect();
                telea::inpaint_planes(&mut p, w, h, hole, 5.0);
                for c in 0..3 {
                    planes[c].copy_from_slice(&p[c]);
                }
                return Ok(());
            }
            continue;
        }
        let targets = target_centers(lv, half);
        let mut nnf = init_nnf(lv, &targets, &valid, li != coarsest, &mut rng);

        let iters = if li == coarsest {
            params.iters_coarse
        } else if li == 0 {
            params.iters_fine
        } else {
            (params.iters_fine + params.iters_coarse) / 2
        };
        for it in 0..iters {
            cancel.check().map_err(|_| AppError::cancelled())?;
            let lv = &levels[li];
            let mut dist: Vec<f32> = targets.iter().zip(&nnf).map(|(&t, &s)| patch_dist(lv, t, s, half, f32::MAX)).collect();
            let passes = if li == 0 { 1 } else { 2 };
            for p in 0..passes {
                propagate_and_search(lv, &targets, &valid, &mut nnf, &mut dist, half, (it + p) % 2 == 1, &mut rng);
            }
            let lv = &mut levels[li];
            vote(lv, &targets, &nnf, &dist, half);
        }
    }

    let l0 = &levels[0];
    for i in 0..w * h {
        if hole[i] {
            for c in 0..3 {
                planes[c][i] = l0.img[i][c].clamp(0.0, 255.0);
            }
        }
    }
    Ok(())
}

fn split_pair(levels: &mut [Level], li: usize) -> (&Level, &mut Level) {
    let (a, b) = levels.split_at_mut(li + 1);
    (&b[0], &mut a[li])
}

fn hole_extent(l: &Level) -> (usize, usize) {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, &b) in l.hole.iter().enumerate() {
        if b {
            let (x, y) = (i % l.w, i / l.w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 == usize::MAX {
        (0, 0)
    } else {
        (x1 - x0 + 1, y1 - y0 + 1)
    }
}

fn downsample(l: &Level) -> Level {
    let (w, h) = ((l.w / 2).max(1), (l.h / 2).max(1));
    let mut img = vec![[0.0; 3]; w * h];
    let mut hole = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0.0f32; 3];
            let mut n = 0.0;
            let mut any_hole = false;
            for dy in 0..2 {
                for dx in 0..2 {
                    let (sx, sy) = ((2 * x + dx).min(l.w - 1), (2 * y + dy).min(l.h - 1));
                    let k = sy * l.w + sx;
                    if l.hole[k] {
                        any_hole = true;
                    } else {
                        for c in 0..3 {
                            acc[c] += l.img[k][c];
                        }
                        n += 1.0;
                    }
                }
            }
            let i = y * w + x;
            hole[i] = any_hole;
            if n > 0.0 {
                img[i] = [acc[0] / n, acc[1] / n, acc[2] / n];
            }
        }
    }
    Level { w, h, img, hole }
}

fn init_with_telea(l: &mut Level) {
    let mut p: Vec<Vec<f32>> = (0..3).map(|c| l.img.iter().map(|px| px[c]).collect()).collect();
    telea::inpaint_planes(&mut p, l.w, l.h, &l.hole, 3.0);
    for (i, px) in l.img.iter_mut().enumerate() {
        *px = [p[0][i], p[1][i], p[2][i]];
    }
}

fn upsample_hole_pixels(coarse: &Level, fine: &mut Level) {
    for y in 0..fine.h {
        for x in 0..fine.w {
            let i = y * fine.w + x;
            if !fine.hole[i] {
                continue;
            }
            // 双线性采样粗层
            let fx = ((x as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (coarse.w - 1) as f32);
            let fy = ((y as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (coarse.h - 1) as f32);
            let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(coarse.w - 1), (y0 + 1).min(coarse.h - 1));
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let p = |xx: usize, yy: usize| coarse.img[yy * coarse.w + xx];
            let mut v = [0.0; 3];
            for c in 0..3 {
                let a = p(x0, y0)[c] * (1.0 - tx) + p(x1, y0)[c] * tx;
                let b = p(x0, y1)[c] * (1.0 - tx) + p(x1, y1)[c] * tx;
                v[c] = a * (1.0 - ty) + b * ty;
            }
            fine.img[i] = v;
        }
    }
}

/// 源块中心：整个块在图内且不含洞像素。
fn valid_sources(l: &Level, half: usize) -> Vec<bool> {
    let (w, h) = (l.w, l.h);
    // 洞的积分图
    let mut ii = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0u32;
        for x in 0..w {
            row += l.hole[y * w + x] as u32;
            ii[(y + 1) * (w + 1) + x + 1] = ii[y * (w + 1) + x + 1] + row;
        }
    }
    let mut v = vec![false; w * h];
    if w <= 2 * half || h <= 2 * half {
        return v;
    }
    for y in half..h - half {
        for x in half..w - half {
            let (x0, y0, x1, y1) = (x - half, y - half, x + half + 1, y + half + 1);
            let s = ii[y1 * (w + 1) + x1] + ii[y0 * (w + 1) + x0] - ii[y0 * (w + 1) + x1] - ii[y1 * (w + 1) + x0];
            v[y * w + x] = s == 0;
        }
    }
    v
}

fn count_valid_sources(l: &Level, half: usize) -> usize {
    valid_sources(l, half).iter().filter(|&&b| b).count()
}

/// 目标块中心：块与洞相交的所有像素。
fn target_centers(l: &Level, half: usize) -> Vec<usize> {
    let (w, h) = (l.w, l.h);
    let mut mark = vec![false; w * h];
    for (i, &b) in l.hole.iter().enumerate() {
        if !b {
            continue;
        }
        let (x, y) = (i % w, i / w);
        for yy in y.saturating_sub(half)..=(y + half).min(h - 1) {
            for xx in x.saturating_sub(half)..=(x + half).min(w - 1) {
                mark[yy * w + xx] = true;
            }
        }
    }
    (0..w * h).filter(|&i| mark[i]).collect()
}

fn random_valid(valid: &[bool], w: usize, h: usize, rng: &mut Rng) -> (i32, i32) {
    for _ in 0..64 {
        let x = rng.range(0, w as i64 - 1) as usize;
        let y = rng.range(0, h as i64 - 1) as usize;
        if valid[y * w + x] {
            return (x as i32, y as i32);
        }
    }
    let i = valid.iter().position(|&b| b).unwrap_or(0);
    ((i % w) as i32, (i / w) as i32)
}

/// 初始化 NNF。细层的结构信息已通过洞区像素上采样传递，这里只需在目标附近做局部随机初始化，
/// 再由 PatchMatch 传播与随机搜索修正。
fn init_nnf(l: &Level, targets: &[usize], valid: &[bool], local: bool, rng: &mut Rng) -> Vec<(i32, i32)> {
    targets
        .iter()
        .map(|&t| {
            let (x, y) = ((t % l.w) as i32, (t / l.w) as i32);
            if local {
                // 在目标附近寻找有效源，保持局部性
                for r in [8i32, 24, 64] {
                    for _ in 0..6 {
                        let sx = (x + rng.range(-(r as i64), r as i64) as i32).clamp(0, l.w as i32 - 1);
                        let sy = (y + rng.range(-(r as i64), r as i64) as i32).clamp(0, l.h as i32 - 1);
                        if valid[sy as usize * l.w + sx as usize] {
                            return (sx, sy);
                        }
                    }
                }
            }
            random_valid(valid, l.w, l.h, rng)
        })
        .collect()
}

#[inline]
fn patch_dist(l: &Level, t: usize, s: (i32, i32), half: usize, cutoff: f32) -> f32 {
    let (tx, ty) = ((t % l.w) as i32, (t / l.w) as i32);
    let hh = half as i32;
    let mut d = 0.0f32;
    let mut n = 0u32;
    for dy in -hh..=hh {
        let yy = ty + dy;
        if yy < 0 || yy >= l.h as i32 {
            continue;
        }
        let sy = (s.1 + dy) as usize;
        for dx in -hh..=hh {
            let xx = tx + dx;
            if xx < 0 || xx >= l.w as i32 {
                continue;
            }
            let a = l.img[yy as usize * l.w + xx as usize];
            let b = l.img[sy * l.w + (s.0 + dx) as usize];
            d += (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2);
            n += 1;
        }
        if d > cutoff * (((2 * half + 1) * (2 * half + 1)) as f32) {
            return f32::MAX;
        }
    }
    if n == 0 {
        f32::MAX
    } else {
        d / n as f32
    }
}

#[allow(clippy::too_many_arguments)]
fn propagate_and_search(
    l: &Level,
    targets: &[usize],
    valid: &[bool],
    nnf: &mut [(i32, i32)],
    dist: &mut [f32],
    half: usize,
    reverse: bool,
    rng: &mut Rng,
) {
    // 目标下标 → 目标序号
    let mut pos = vec![u32::MAX; l.w * l.h];
    for (k, &t) in targets.iter().enumerate() {
        pos[t] = k as u32;
    }
    let is_valid =
        |s: (i32, i32)| s.0 >= 0 && s.1 >= 0 && (s.0 as usize) < l.w && (s.1 as usize) < l.h && valid[s.1 as usize * l.w + s.0 as usize];
    let order: Box<dyn Iterator<Item = usize>> = if reverse { Box::new((0..targets.len()).rev()) } else { Box::new(0..targets.len()) };
    let step: i32 = if reverse { 1 } else { -1 };
    let max_r = l.w.max(l.h) as i64;
    for k in order {
        let t = targets[k];
        let (tx, ty) = ((t % l.w) as i32, (t / l.w) as i32);
        let mut best = nnf[k];
        let mut bd = dist[k];
        // 传播：来自左/上（正向）或右/下（反向）邻居
        for (nx, ny) in [(tx + step, ty), (tx, ty + step)] {
            if nx < 0 || ny < 0 || nx >= l.w as i32 || ny >= l.h as i32 {
                continue;
            }
            let nk = pos[ny as usize * l.w + nx as usize];
            if nk == u32::MAX {
                continue;
            }
            let ns = nnf[nk as usize];
            let cand = (ns.0 - (nx - tx), ns.1 - (ny - ty));
            if is_valid(cand) {
                let d = patch_dist(l, t, cand, half, bd);
                if d < bd {
                    bd = d;
                    best = cand;
                }
            }
        }
        // 随机搜索：半径指数衰减
        let mut r = max_r;
        while r >= 1 {
            let cand = (
                (best.0 as i64 + rng.range(-r, r)).clamp(0, l.w as i64 - 1) as i32,
                (best.1 as i64 + rng.range(-r, r)).clamp(0, l.h as i64 - 1) as i32,
            );
            if is_valid(cand) {
                let d = patch_dist(l, t, cand, half, bd);
                if d < bd {
                    bd = d;
                    best = cand;
                }
            }
            r /= 2;
        }
        nnf[k] = best;
        dist[k] = bd;
    }
}

fn vote(l: &mut Level, targets: &[usize], nnf: &[(i32, i32)], dist: &[f32], half: usize) {
    let mut ds: Vec<f32> = dist.iter().copied().filter(|d| d.is_finite() && *d < f32::MAX).collect();
    let sigma2 = if ds.is_empty() {
        1.0
    } else {
        let k = (ds.len() * 3) / 4;
        ds.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap());
        ds[k].max(1.0)
    };
    let (w, h) = (l.w, l.h);
    let mut acc = vec![[0.0f64; 4]; w * h];
    let hh = half as i32;
    for (k, &t) in targets.iter().enumerate() {
        let d = dist[k];
        if !(d < f32::MAX) {
            continue;
        }
        let wgt = (-(d as f64) / (2.0 * sigma2 as f64)).exp().max(1e-8);
        let (tx, ty) = ((t % w) as i32, (t / w) as i32);
        let s = nnf[k];
        for dy in -hh..=hh {
            let yy = ty + dy;
            if yy < 0 || yy >= h as i32 {
                continue;
            }
            for dx in -hh..=hh {
                let xx = tx + dx;
                if xx < 0 || xx >= w as i32 {
                    continue;
                }
                let i = yy as usize * w + xx as usize;
                if !l.hole[i] {
                    continue;
                }
                let v = l.img[(s.1 + dy) as usize * w + (s.0 + dx) as usize];
                let a = &mut acc[i];
                a[0] += wgt * v[0] as f64;
                a[1] += wgt * v[1] as f64;
                a[2] += wgt * v[2] as f64;
                a[3] += wgt;
            }
        }
    }
    for i in 0..w * h {
        if l.hole[i] && acc[i][3] > 0.0 {
            let a = acc[i];
            l.img[i] = [(a[0] / a[3]) as f32, (a[1] / a[3]) as f32, (a[2] / a[3]) as f32];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 规则条纹纹理：样本块合成应比扩散更好地延续条纹。
    #[test]
    fn restores_periodic_stripes_better_than_telea() {
        let (w, h) = (96, 64);
        let truth: Vec<f32> = (0..w * h).map(|i| if (i % w) / 4 % 2 == 0 { 40.0 } else { 210.0 }).collect();
        let mut hole = vec![false; w * h];
        for y in 24..40 {
            for x in 36..60 {
                hole[y * w + x] = true;
            }
        }
        let damaged: Vec<f32> = truth.iter().zip(&hole).map(|(&v, &b)| if b { 128.0 } else { v }).collect();

        let mut ps = vec![damaged.clone(), damaged.clone(), damaged.clone()];
        synthesize(&mut ps, w, h, &hole, SynthParams::default(), &CancellationToken::new()).unwrap();
        let mut pt = vec![damaged.clone(), damaged.clone(), damaged];
        telea::inpaint_planes(&mut pt, w, h, &hole, 5.0);

        let err = |p: &Vec<f32>| -> f32 {
            hole.iter().enumerate().filter(|(_, &b)| b).map(|(i, _)| (p[i] - truth[i]).abs()).sum::<f32>()
                / hole.iter().filter(|&&b| b).count() as f32
        };
        let (es, et) = (err(&ps[0]), err(&pt[0]));
        assert!(es < et, "synth {es} vs telea {et}");
        assert!(es < 40.0, "synth err {es}");
    }

    #[test]
    fn deterministic() {
        let (w, h) = (48, 48);
        let base: Vec<f32> = (0..w * h).map(|i| ((i % w) * 5 + (i / w) * 3) as f32 % 255.0).collect();
        let mut hole = vec![false; w * h];
        for y in 18..30 {
            for x in 18..30 {
                hole[y * w + x] = true;
            }
        }
        let run = || {
            let mut p = vec![base.clone(), base.clone(), base.clone()];
            synthesize(&mut p, w, h, &hole, SynthParams::default(), &CancellationToken::new()).unwrap();
            p
        };
        assert_eq!(run(), run());
    }
}
