//! Telea (2004) Fast Marching 修复，行为对齐 OpenCV `INPAINT_TELEA`。
//!
//! 用于小水印、纯色、天空、墙面与简单纹理（规格 §7 FastInpaint）。
//! 也作为纹理合成的粗层初始化与 AlphaRestore 不稳定像素的兜底。

use std::cmp::Ordering;
use std::collections::BinaryHeap;

const KNOWN: u8 = 0;
const BAND: u8 = 1;
const INSIDE: u8 = 2;
const INF: f32 = 1.0e6;

#[derive(Copy, Clone, PartialEq)]
struct Item {
    t: f32,
    i: usize,
}
impl Eq for Item {}
impl Ord for Item {
    fn cmp(&self, o: &Self) -> Ordering {
        // 最小堆
        o.t.partial_cmp(&self.t).unwrap_or(Ordering::Equal).then(o.i.cmp(&self.i))
    }
}
impl PartialOrd for Item {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// 对 `channels` 个浮点平面（行主序 w×h）原地修复 `hole` 为 true 的像素。
pub fn inpaint_planes(planes: &mut [Vec<f32>], w: usize, h: usize, hole: &[bool], radius: f32) {
    if w == 0 || h == 0 || !hole.iter().any(|&b| b) {
        return;
    }
    let n = w * h;
    let mut flag = vec![KNOWN; n];
    let mut t = vec![0.0f32; n];
    let mut heap = BinaryHeap::new();

    for i in 0..n {
        if hole[i] {
            flag[i] = INSIDE;
            t[i] = INF;
        }
    }
    // 边界带：与洞相邻的已知像素
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if flag[i] != KNOWN {
                continue;
            }
            let near_hole = neighbors4(x, y, w, h).any(|j| flag[j] == INSIDE);
            if near_hole {
                flag[i] = BAND;
                heap.push(Item { t: 0.0, i });
            }
        }
    }

    let r = radius.max(1.0);
    let ri = r.ceil() as i64;
    while let Some(Item { i, .. }) = heap.pop() {
        if flag[i] == KNOWN {
            continue;
        }
        flag[i] = KNOWN;
        let (x, y) = (i % w, i / w);
        for j in neighbors4(x, y, w, h).collect::<Vec<_>>() {
            if flag[j] != INSIDE {
                continue;
            }
            let (jx, jy) = (j % w, j / w);
            // 求解 Eikonal 得到到达时间
            let tj = solve_eikonal(jx, jy, w, h, &t, &flag);
            t[j] = tj;
            inpaint_pixel(planes, w, h, jx, jy, &t, &flag, r, ri);
            flag[j] = BAND;
            heap.push(Item { t: tj, i: j });
        }
    }
}

fn neighbors4(x: usize, y: usize, w: usize, h: usize) -> impl Iterator<Item = usize> {
    let mut v = [usize::MAX; 4];
    if x > 0 {
        v[0] = y * w + x - 1;
    }
    if x + 1 < w {
        v[1] = y * w + x + 1;
    }
    if y > 0 {
        v[2] = (y - 1) * w + x;
    }
    if y + 1 < h {
        v[3] = (y + 1) * w + x;
    }
    v.into_iter().filter(|&i| i != usize::MAX)
}

fn solve_pair(t1: f32, t2: f32) -> f32 {
    if t1 >= INF && t2 >= INF {
        return INF;
    }
    let (a, b) = (t1.min(t2), t1.max(t2));
    if b >= INF {
        return a + 1.0;
    }
    let d = 2.0 - (a - b) * (a - b);
    if d > 0.0 {
        let s = ((a + b) + d.sqrt()) / 2.0;
        if s >= b {
            return s;
        }
    }
    a + 1.0
}

fn solve_eikonal(x: usize, y: usize, w: usize, h: usize, t: &[f32], flag: &[u8]) -> f32 {
    let get = |xx: i64, yy: i64| -> f32 {
        if xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
            return INF;
        }
        let k = yy as usize * w + xx as usize;
        if flag[k] == INSIDE {
            INF
        } else {
            t[k]
        }
    };
    let (x, y) = (x as i64, y as i64);
    let a = solve_pair(get(x - 1, y), get(x, y - 1));
    let b = solve_pair(get(x + 1, y), get(x, y - 1));
    let c = solve_pair(get(x - 1, y), get(x, y + 1));
    let d = solve_pair(get(x + 1, y), get(x, y + 1));
    a.min(b).min(c).min(d)
}

#[allow(clippy::too_many_arguments)]
fn inpaint_pixel(planes: &mut [Vec<f32>], w: usize, h: usize, x: usize, y: usize, t: &[f32], flag: &[u8], r: f32, ri: i64) {
    let i = y * w + x;
    // 到达时间梯度（法向）
    let tg = |xx: i64, yy: i64| -> Option<f32> {
        if xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
            return None;
        }
        let k = yy as usize * w + xx as usize;
        (flag[k] != INSIDE).then_some(t[k])
    };
    let (xi, yi) = (x as i64, y as i64);
    let gx = match (tg(xi + 1, yi), tg(xi - 1, yi)) {
        (Some(a), Some(b)) => (a - b) * 0.5,
        (Some(a), None) => a - t[i],
        (None, Some(b)) => t[i] - b,
        _ => 0.0,
    };
    let gy = match (tg(xi, yi + 1), tg(xi, yi - 1)) {
        (Some(a), Some(b)) => (a - b) * 0.5,
        (Some(a), None) => a - t[i],
        (None, Some(b)) => t[i] - b,
        _ => 0.0,
    };

    let nch = planes.len();
    let mut acc = [0.0f64; 4];
    let mut wsum = 0.0f64;
    for ky in (yi - ri).max(0)..=(yi + ri).min(h as i64 - 1) {
        for kx in (xi - ri).max(0)..=(xi + ri).min(w as i64 - 1) {
            let k = ky as usize * w + kx as usize;
            if flag[k] == INSIDE {
                continue;
            }
            let (rx, ry) = ((xi - kx) as f32, (yi - ky) as f32);
            let l2 = rx * rx + ry * ry;
            if l2 > r * r || l2 == 0.0 {
                continue;
            }
            let l = l2.sqrt();
            let dir = ((rx * gx + ry * gy).abs() / l).max(1e-6);
            let dst = 1.0 / (l2 * l);
            let lev = 1.0 / (1.0 + (t[k] - t[i]).abs());
            let wgt = (dir * dst * lev) as f64;
            for c in 0..nch {
                // 一阶近似：I(q) + ∇I(q)·(p−q)
                let p = &planes[c];
                let gxq = grad_known(p, flag, w, h, kx, ky, 1, 0);
                let gyq = grad_known(p, flag, w, h, kx, ky, 0, 1);
                acc[c] += wgt * (p[k] + gxq * rx + gyq * ry) as f64;
            }
            wsum += wgt;
        }
    }
    if wsum > 0.0 {
        for (c, plane) in planes.iter_mut().enumerate() {
            plane[i] = (acc[c] / wsum).clamp(0.0, 255.0) as f32;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn grad_known(p: &[f32], flag: &[u8], w: usize, h: usize, x: i64, y: i64, dx: i64, dy: i64) -> f32 {
    let ok = |xx: i64, yy: i64| xx >= 0 && yy >= 0 && xx < w as i64 && yy < h as i64 && flag[yy as usize * w + xx as usize] != INSIDE;
    let at = |xx: i64, yy: i64| p[yy as usize * w + xx as usize];
    match (ok(x + dx, y + dy), ok(x - dx, y - dy)) {
        (true, true) => (at(x + dx, y + dy) - at(x - dx, y - dy)) * 0.5,
        (true, false) => at(x + dx, y + dy) - at(x, y),
        (false, true) => at(x, y) - at(x - dx, y - dy),
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_constant_background_exactly() {
        let (w, h) = (40, 30);
        let mut p = vec![vec![120.0f32; w * h]];
        let mut hole = vec![false; w * h];
        for y in 10..20 {
            for x in 12..28 {
                p[0][y * w + x] = 255.0;
                hole[y * w + x] = true;
            }
        }
        inpaint_planes(&mut p, w, h, &hole, 5.0);
        assert!(p[0].iter().all(|&v| (v - 120.0).abs() < 0.5));
    }

    #[test]
    fn continues_horizontal_gradient() {
        let (w, h) = (60, 20);
        let mut p = vec![(0..w * h).map(|i| (i % w) as f32 * 3.0).collect::<Vec<_>>()];
        let truth = p[0].clone();
        let mut hole = vec![false; w * h];
        for y in 5..15 {
            for x in 25..35 {
                hole[y * w + x] = true;
                p[0][y * w + x] = 0.0;
            }
        }
        inpaint_planes(&mut p, w, h, &hole, 5.0);
        let err: f32 = hole.iter().enumerate().filter(|(_, &b)| b).map(|(i, _)| (p[0][i] - truth[i]).abs()).sum::<f32>() / 100.0;
        assert!(err < 6.0, "mean abs err {err}");
    }
}
