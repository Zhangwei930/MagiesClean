//! 本地 AI Inpainting（LaMa 兼容 ONNX 等）：原分辨率分块修复 + 按区域缓存。
//!
//! - 在原图分辨率下以模型输入尺寸为窗口逐块修复：大面积 Mask 不整体缩放到模型尺寸
//!   （缩小再放大会让填充区明显发糊）。每个 Mask 区域切成若干单元，窗口以单元为中心、
//!   只写回单元内的洞；按从上到下、从左到右的顺序处理，已修复的像素成为后续窗口的上下文。
//! - 每个区域（一个候选或手动绘制区域）独立修复：只读取原图，文件内所有候选区域
//!   （`RemovalOptions::unknown`，含未选择去除的）都不作为上下文。因此区域的结果只取决于
//!   原图像素与这些 Mask，可按内容哈希缓存——增减候选时只计算新增区域，
//!   后台也可以提前为待复核候选算好结果。

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Condvar, Mutex};

use wm_core::decision::RemovalRoute;
use wm_core::msg;
use wm_core::traits::{AiInpaintBackend, RemovalOptions, WatermarkRemover};
use wm_core::{AppError, GrayU8, ImageBuffer, MaskRegion, PixelRect, Result, Tensor, WatermarkMask};
use wm_image::ops;

/// 缓存键版本：分块策略或合成方式改变时递增，旧缓存自然失效。
const CACHE_VERSION: u32 = 1;

/// 单元边长占窗口的比例，按洞的厚度（洞内像素到最近背景的最大距离 / 窗口边长）分档：
/// 细笔画洞的每个像素附近本来就有背景，单元可以接近整个窗口，推理次数更少；
/// 大块实心洞需要单元四周留足上下文（0.6，四周各 20%）。
const AI_CELL_TIERS: [(f32, f32); 3] = [(0.08, 0.9), (0.14, 0.75), (f32::INFINITY, 0.6)];

/// 一个区域的修复结果：`rect` 内 `written > 0` 的像素为结果（已按软 Mask 与原图混合）。
#[derive(Debug)]
pub struct Patch {
    rect: PixelRect,
    pixels: ImageBuffer,
    written: GrayU8,
}

impl Patch {
    fn bytes(&self) -> usize {
        self.pixels.data.len() + self.written.data.len()
    }

    fn apply(&self, image: &mut ImageBuffer) {
        for y in 0..self.rect.height {
            for x in 0..self.rect.width {
                if self.written.get(x, y) == 0 {
                    continue;
                }
                let i = image.idx(self.rect.x + x, self.rect.y + y);
                let j = self.pixels.idx(x, y);
                image.data[i..i + 3].copy_from_slice(&self.pixels.data[j..j + 3]);
            }
        }
    }
}

#[derive(Default)]
struct CacheInner {
    map: HashMap<u64, Arc<Patch>>,
    order: VecDeque<u64>,
    bytes: usize,
    pending: HashSet<u64>,
}

/// 区域修复结果缓存（按内容哈希，先进先出淘汰）。同一区域正在被另一线程计算时等待其结果，
/// 不重复推理——后台预计算与用户触发的预览可以同时进行。
pub struct PatchCache {
    budget: usize,
    inner: Mutex<CacheInner>,
    ready: Condvar,
}

impl PatchCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self { budget: budget_bytes, inner: Mutex::new(CacheInner::default()), ready: Condvar::new() }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.map.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 取缓存；未命中时计算。另一线程正在计算同一键时等待它完成。
    fn get_or_compute(&self, key: u64, compute: impl FnOnce() -> Result<Patch>) -> Result<Arc<Patch>> {
        {
            let mut g = self.inner.lock().expect("patch cache poisoned");
            loop {
                if let Some(p) = g.map.get(&key) {
                    return Ok(p.clone());
                }
                if !g.pending.contains(&key) {
                    g.pending.insert(key);
                    break;
                }
                g = self.ready.wait(g).expect("patch cache poisoned");
            }
        }
        // 计算失败或被取消时撤销占位，唤醒等待者自行计算
        struct Pending<'a>(&'a PatchCache, u64);
        impl Drop for Pending<'_> {
            fn drop(&mut self) {
                if let Ok(mut g) = self.0.inner.lock() {
                    g.pending.remove(&self.1);
                }
                self.0.ready.notify_all();
            }
        }
        let _pending = Pending(self, key);
        let patch = Arc::new(compute()?);
        let mut g = self.inner.lock().expect("patch cache poisoned");
        g.bytes += patch.bytes();
        g.map.insert(key, patch.clone());
        g.order.push_back(key);
        while g.bytes > self.budget && g.order.len() > 1 {
            let Some(old) = g.order.pop_front() else { break };
            if let Some(p) = g.map.remove(&old) {
                g.bytes -= p.bytes();
            }
        }
        Ok(patch)
    }
}

/// 一个待修复区域的分块计划。
struct Plan {
    /// 区域自身的软 Mask（全图坐标）。
    region: MaskRegion,
    /// (单元, 窗口)，按从上到下、从左到右排列；只含有洞的单元。
    tiles: Vec<(PixelRect, PixelRect)>,
    /// 全部窗口的外接矩形。
    bounds: PixelRect,
}

pub struct AiInpaintRemover {
    pub backend: Arc<dyn AiInpaintBackend>,
    pub cache: Option<Arc<PatchCache>>,
}

impl AiInpaintRemover {
    pub fn new(backend: Arc<dyn AiInpaintBackend>, cache: Option<Arc<PatchCache>>) -> Self {
        Self { backend, cache }
    }

    /// 以 `cell` 为中心、模型输入尺寸的窗口；图像不足一个窗口时取整幅宽/高。
    fn window(cell: &PixelRect, (mw, mh): (u32, u32), iw: u32, ih: u32) -> PixelRect {
        let (ww, wh) = (mw.min(iw), mh.min(ih));
        let cx = cell.x + cell.width / 2;
        let cy = cell.y + cell.height / 2;
        let x0 = cx.saturating_sub(ww / 2).min(iw - ww);
        let y0 = cy.saturating_sub(wh / 2).min(ih - wh);
        PixelRect::new(x0, y0, ww, wh)
    }

    /// 区域分块：按洞的厚度选单元大小，只保留含洞的单元。
    fn plan(&self, region: &MaskRegion, iw: u32, ih: u32) -> Option<Plan> {
        let (mw, mh) = self.backend.input_size();
        let b = region.data.nonzero_bounds()?;
        let core = PixelRect::new(region.rect.x + b.x, region.rect.y + b.y, b.width, b.height).pad(1, iw, ih);
        // 外扩 1 px 后可能超出区域自身的数据范围：按全图坐标栅格化到 core
        let mut single = WatermarkMask::new(String::new(), String::new(), iw, ih);
        single.regions.push(region.clone());
        let local = single.rasterize(&core);
        let known =
            GrayU8 { width: local.width, height: local.height, data: local.data.iter().map(|&v| if v > 0 { 0 } else { 255 }).collect() };
        let thickness = ops::distance_to_nonzero(&known).data.iter().fold(0.0f32, |a, &v| a.max(v));
        let side = mw.min(mh) as f32;
        let ratio = AI_CELL_TIERS.iter().find(|(t, _)| thickness <= side * t).map_or(0.6, |(_, r)| *r);
        let cell = ((side * ratio) as u32).max(1);
        let (nx, ny) = (core.width.div_ceil(cell), core.height.div_ceil(cell));
        let (cw, ch) = (core.width.div_ceil(nx), core.height.div_ceil(ny));
        let mut tiles = Vec::new();
        for gy in 0..ny {
            for gx in 0..nx {
                let (x0, y0) = (core.x + gx * cw, core.y + gy * ch);
                let c = PixelRect::new(x0, y0, cw.min(core.right() - x0), ch.min(core.bottom() - y0));
                let has_hole = (c.y..c.bottom()).any(|y| (c.x..c.right()).any(|x| local.get(x - core.x, y - core.y) > 0));
                if has_hole {
                    tiles.push((c, Self::window(&c, (mw, mh), iw, ih)));
                }
            }
        }
        let bounds = tiles.iter().map(|t| t.1).reduce(|a, b| a.union(&b))?;
        tracing::debug!(?core, thickness, cell, tiles = tiles.len(), "ai inpaint plan");
        Some(Plan { region: region.clone(), tiles, bounds })
    }

    /// 结果只取决于：分块计划、窗口内的原图像素、窗口内的“不可用作上下文”Mask 与区域自身的软 Mask。
    fn key(&self, plan: &Plan, image: &ImageBuffer, unknown: &GrayU8, own: &GrayU8) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (CACHE_VERSION, self.backend.input_size(), (image.width, image.height)).hash(&mut h);
        let b = plan.bounds;
        for (c, w) in &plan.tiles {
            (c.x, c.y, c.width, c.height, w.x, w.y, w.width, w.height).hash(&mut h);
        }
        for y in b.y..b.bottom() {
            let i = image.idx(b.x, y);
            h.write(&image.data[i..i + b.width as usize * 4]);
            let j = ((y - b.y) * b.width) as usize;
            h.write(&own.data[j..j + b.width as usize]);
            for v in &unknown.data[j..j + b.width as usize] {
                h.write_u8((*v > 0) as u8);
            }
        }
        h.finish()
    }

    /// 对一个窗口推理，返回窗口尺寸的修复结果。`hole` 为窗口坐标下待修复像素。
    fn infer(&self, image: &ImageBuffer, win: &PixelRect, hole: &GrayU8) -> Result<ImageBuffer> {
        let (mw, mh) = self.backend.input_size();
        let crop = image.crop(win);
        let resized = win.width != mw || win.height != mh;
        let (small, small_hole) =
            if resized { (ops::resize(&crop, mw, mh), ops::resize_mask_nearest(hole, mw, mh)) } else { (crop, hole.clone()) };
        let plane = (mw * mh) as usize;
        let mut img_t = Tensor::zeros(vec![1, 3, mh as usize, mw as usize]);
        let mut mask_t = Tensor::zeros(vec![1, 1, mh as usize, mw as usize]);
        for i in 0..plane {
            let h = small_hole.data[i] > 0;
            mask_t.data[i] = if h { 1.0 } else { 0.0 };
            for c in 0..3 {
                img_t.data[c * plane + i] = if h { 0.0 } else { small.data[i * 4 + c] as f32 / 255.0 };
            }
        }
        let out = self.backend.inpaint(&img_t, &mask_t)?;
        if out.data.len() != 3 * plane {
            return Err(AppError::inference(msg!(
                "AI 修复模型输出尺寸不符合契约",
                "AI inpainting output size does not match the contract"
            )));
        }
        let mut out_img = ImageBuffer::new(mw, mh);
        for i in 0..plane {
            for c in 0..3 {
                out_img.data[i * 4 + c] = (out.data[c * plane + i] * 255.0).round().clamp(0.0, 255.0) as u8;
            }
            out_img.data[i * 4 + 3] = 255;
        }
        Ok(if resized { ops::resize(&out_img, win.width, win.height) } else { out_img })
    }

    /// 在原图的局部副本上逐块修复一个区域。
    fn compute(&self, plan: &Plan, image: &ImageBuffer, unknown: &GrayU8, own: &GrayU8, options: &RemovalOptions) -> Result<Patch> {
        let b = plan.bounds;
        let mut local = image.crop(&b);
        let mut written = GrayU8::new(b.width, b.height);
        let at = |x: u32, y: u32| ((y - b.y) * b.width + (x - b.x)) as usize;
        for (c, win) in &plan.tiles {
            options.cancel.check()?;
            // 模型输入的洞：所有候选区域（不可作为上下文）减去本区域已修复的像素
            let mut hole = GrayU8::new(win.width, win.height);
            for y in win.y..win.bottom() {
                for x in win.x..win.right() {
                    let i = at(x, y);
                    if (unknown.data[i] > 0 || own.data[i] > 0) && written.data[i] == 0 {
                        hole.set(x - win.x, y - win.y, 255);
                    }
                }
            }
            // 洞略微外扩：抗锯齿边缘的残留不作为上下文
            let hole = ops::dilate(&hole, 2.0);
            let local_win = PixelRect::new(win.x - b.x, win.y - b.y, win.width, win.height);
            let out = self.infer(&local, &local_win, &hole)?;
            // 只按软 Mask 写回单元内属于本区域、尚未写过的像素
            for y in c.y..c.bottom() {
                for x in c.x..c.right() {
                    let i = at(x, y);
                    let m = own.data[i] as u32;
                    if m == 0 || written.data[i] > 0 {
                        continue;
                    }
                    let li = local.idx(x - b.x, y - b.y);
                    let oi = out.idx(x - win.x, y - win.y);
                    for k in 0..3 {
                        let o = local.data[li + k] as u32;
                        let p = out.data[oi + k] as u32;
                        local.data[li + k] = ((o * (255 - m) + p * m + 127) / 255) as u8;
                    }
                    written.data[i] = own.data[i];
                }
            }
        }
        Ok(Patch { rect: b, pixels: local, written })
    }

    /// 计算（或取缓存）每个区域的修复结果。只读取 `image`，不修改。
    fn patches(&self, image: &ImageBuffer, mask: &WatermarkMask, options: &RemovalOptions) -> Result<Vec<Arc<Patch>>> {
        let mut out = Vec::new();
        for region in &mask.regions {
            options.cancel.check()?;
            let Some(plan) = self.plan(region, image.width, image.height) else { continue };
            let b = plan.bounds;
            let own = WatermarkMask { regions: vec![plan.region.clone()], ..mask.clone() }.rasterize(&b);
            let unknown = match &options.unknown {
                Some(u) => u.rasterize(&b),
                None => mask.rasterize(&b),
            };
            let run = || self.compute(&plan, image, &unknown, &own, options);
            let patch = match &self.cache {
                Some(cache) => cache.get_or_compute(self.key(&plan, image, &unknown, &own), run)?,
                None => Arc::new(run()?),
            };
            out.push(patch);
        }
        Ok(out)
    }

    /// 只计算并缓存结果，不修改图像（后台预计算）。
    pub fn precompute(&self, image: &ImageBuffer, mask: &WatermarkMask, options: &RemovalOptions) -> Result<()> {
        self.patches(image, mask, options).map(|_| ())
    }
}

impl WatermarkRemover for AiInpaintRemover {
    fn route(&self) -> RemovalRoute {
        RemovalRoute::AiInpaint
    }
    fn remove(&self, image: &mut ImageBuffer, mask: &WatermarkMask, options: &RemovalOptions) -> Result<()> {
        // 先全部从原图计算，再统一写回：区域之间互不影响，结果与处理顺序、缓存命中无关
        let patches = self.patches(image, mask, options)?;
        for p in &patches {
            p.apply(image);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn mask_block(w: u32, h: u32, r: PixelRect) -> WatermarkMask {
        let mut m = WatermarkMask::new("m".into(), "f".into(), w, h);
        let mut d = GrayU8::new(r.width, r.height);
        d.data.fill(255);
        m.regions.push(MaskRegion::new(r, d, None));
        m
    }

    /// 返回常量灰色并记录调用次数与输入形状。
    struct GrayBackend(AtomicUsize);
    impl AiInpaintBackend for GrayBackend {
        fn input_size(&self) -> (u32, u32) {
            (32, 32)
        }
        fn inpaint(&self, image: &Tensor, mask: &Tensor) -> Result<Tensor> {
            assert_eq!(image.shape, vec![1, 3, 32, 32]);
            assert_eq!(mask.shape, vec![1, 1, 32, 32]);
            self.0.fetch_add(1, Ordering::Relaxed);
            let mut t = image.clone();
            t.data.fill(0.5);
            Ok(t)
        }
    }

    struct EchoBackend;
    impl AiInpaintBackend for EchoBackend {
        fn input_size(&self) -> (u32, u32) {
            (32, 32)
        }
        fn inpaint(&self, image: &Tensor, _mask: &Tensor) -> Result<Tensor> {
            Ok(image.clone())
        }
    }

    #[test]
    fn ai_remover_respects_tensor_contract() {
        let mut img = ImageBuffer::filled(64, 64, [200, 100, 50, 255]);
        let mask = mask_block(64, 64, PixelRect::new(20, 20, 10, 10));
        AiInpaintRemover::new(Arc::new(EchoBackend), None).remove(&mut img, &mask, &RemovalOptions::default()).unwrap();
        // 输出只在 Mask 内变化
        assert_eq!(img.get(0, 0), [200, 100, 50, 255]);
    }

    #[test]
    fn ai_remover_tiles_large_masks_at_native_resolution() {
        let orig = ImageBuffer::filled(200, 120, [200, 100, 50, 255]);
        let rect = PixelRect::new(20, 20, 150, 80);
        let mask = mask_block(200, 120, rect);
        let backend = Arc::new(GrayBackend(Default::default()));
        let mut img = orig.clone();
        AiInpaintRemover::new(backend.clone(), None).remove(&mut img, &mask, &RemovalOptions::default()).unwrap();
        // Mask 大于窗口：分多块推理，且每个洞像素都被填充、Mask 外不变
        assert!(backend.0.load(Ordering::Relaxed) > 4);
        for y in 0..120 {
            for x in 0..200 {
                let want = if rect.contains(x, y) { [128, 128, 128, 255] } else { [200, 100, 50, 255] };
                assert_eq!(img.get(x, y), want, "({x},{y})");
            }
        }
    }

    #[test]
    fn cached_regions_are_not_inferred_again() {
        let img = ImageBuffer::filled(400, 300, [90, 120, 60, 255]);
        let a = mask_block(400, 300, PixelRect::new(20, 20, 12, 8));
        let b = mask_block(400, 300, PixelRect::new(300, 240, 12, 8));
        let mut both = a.clone();
        both.regions.extend(b.regions.clone());
        let unknown = Arc::new(both.clone());
        let opts = RemovalOptions { unknown: Some(unknown), ..Default::default() };
        let backend = Arc::new(GrayBackend(Default::default()));
        let cache = Arc::new(PatchCache::new(64 << 20));
        let r = AiInpaintRemover::new(backend.clone(), Some(cache.clone()));

        // 先只去除 A，再去除 A+B：第二次只推理 B
        let mut first = img.clone();
        r.remove(&mut first, &a, &opts).unwrap();
        let after_a = backend.0.load(Ordering::Relaxed);
        let mut second = img.clone();
        r.remove(&mut second, &both, &opts).unwrap();
        assert_eq!(backend.0.load(Ordering::Relaxed), after_a * 2);
        assert_eq!(cache.len(), 2);

        // 预计算后的区域在去除时直接命中
        let fresh = Arc::new(PatchCache::new(64 << 20));
        let r2 = AiInpaintRemover::new(backend.clone(), Some(fresh));
        r2.precompute(&img, &a, &opts).unwrap();
        r2.precompute(&img, &b, &opts).unwrap();
        let before = backend.0.load(Ordering::Relaxed);
        let mut third = img.clone();
        r2.remove(&mut third, &both, &opts).unwrap();
        assert_eq!(backend.0.load(Ordering::Relaxed), before);
        assert_eq!(third.data, second.data);
    }
}
