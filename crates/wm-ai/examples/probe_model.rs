//! 打印 ONNX 模型的输入输出签名，并用一次合成推理检查输出取值范围。
//!
//! ```text
//! cargo run -p wm-ai --release --example probe_model -- models/inpainting/lama.onnx
//! ```

use ort::session::Session;
use ort::value::Tensor;

fn main() {
    let path = std::env::args().nth(1).expect("usage: probe_model <model.onnx>");
    let threads: usize = std::env::var("THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
    let builder = Session::builder().and_then(|b| b.with_intra_threads(threads)).expect("builder");
    let t_load = std::time::Instant::now();
    let mut session = builder.commit_from_file(&path).expect("load model");
    println!("load {} ms, threads {threads}", t_load.elapsed().as_millis());
    for i in &session.inputs {
        println!("input  {:<10} {:?}", i.name, i.input_type);
    }
    for o in &session.outputs {
        println!("output {:<10} {:?}", o.name, o.output_type);
    }
    if let Ok(meta) = session.metadata() {
        for k in meta.custom_keys().unwrap_or_default() {
            let v = meta.custom(&k).ok().flatten().unwrap_or_default();
            println!("metadata {k} = {}…（共 {} 字符）", v.chars().take(40).collect::<String>(), v.chars().count());
        }
    }
    // 以下合成推理只适用于 image + mask 两个输入的修复模型
    if session.inputs.len() != 2 {
        return;
    }
    // 512×512 渐变图 + 中央方块 Mask
    let (w, h) = (512usize, 512usize);
    let mut img = vec![0f32; 3 * w * h];
    let mut mask = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            img[i] = x as f32 / w as f32;
            img[w * h + i] = y as f32 / h as f32;
            img[2 * w * h + i] = 0.5;
            if (192..320).contains(&x) && (192..320).contains(&y) {
                mask[i] = 1.0;
                for c in 0..3 {
                    img[c * w * h + i] = 0.0;
                }
            }
        }
    }
    let names: Vec<String> = session.inputs.iter().map(|i| i.name.clone()).collect();
    for _ in 0..2 {
        let t0 = std::time::Instant::now();
        session
            .run(vec![
                (names[0].clone(), Tensor::from_array((vec![1i64, 3, h as i64, w as i64], img.clone())).unwrap()),
                (names[1].clone(), Tensor::from_array((vec![1i64, 1, h as i64, w as i64], mask.clone())).unwrap()),
            ])
            .expect("run");
        println!("inference {} ms", t0.elapsed().as_millis());
    }
    let t0 = std::time::Instant::now();
    let out = session
        .run(vec![
            (names[0].clone(), Tensor::from_array((vec![1i64, 3, h as i64, w as i64], img)).unwrap()),
            (names[1].clone(), Tensor::from_array((vec![1i64, 1, h as i64, w as i64], mask)).unwrap()),
        ])
        .expect("run");
    println!("inference {} ms", t0.elapsed().as_millis());
    for (name, v) in out.iter() {
        let (shape, data) = v.try_extract_tensor::<f32>().unwrap();
        let (mn, mx) = data.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        // 中心像素（被 Mask 覆盖）与左上角像素（未覆盖，R = 0）
        let c = 256 * w + 256;
        println!(
            "{name} shape={shape:?} min={mn:.3} max={mx:.3} center_rgb=({:.3},{:.3},{:.3}) corner_r={:.3}",
            data[c],
            data[w * h + c],
            data[2 * w * h + c],
            data[0]
        );
    }
}
