//! 对一张图片运行文字识别并打印结果（调试用）：ocr_probe <图片>
fn main() {
    let path = std::env::args().nth(1).expect("ocr_probe <图片>");
    let m = wm_ai::ModelManager::open(std::path::Path::new("models"));
    let ocr = m.ocr().expect("文字识别模型未就绪");
    let img = wm_image::decode(std::path::Path::new(&path), None).unwrap().buffer;
    let (img, _) = wm_image::ops::thumbnail(&img, wm_image::DETECTION_MAX_SIDE);
    let t = std::time::Instant::now();
    let lines = wm_core::traits::OcrEngine::recognize(ocr.as_ref(), &img).unwrap();
    println!("识别 {} 行，用时 {} ms", lines.len(), t.elapsed().as_millis());
    for l in lines {
        println!("  {:>4.0},{:>4.0} {:>4.0}x{:<3.0} {:.2}  {}", l.bbox.x, l.bbox.y, l.bbox.width, l.bbox.height, l.score, l.text);
    }
}
