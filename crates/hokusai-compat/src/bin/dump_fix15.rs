//! Dump the CPU stroke result as RGBA u16 fix15 for the C differential gate.
//!
//! Usage: dump_fix15 <brush.myb> <stroke.json> <out.bin> [linear]
//!
//! The event sequence mirrors cref/ref_runner.c:
//! fresh state -> dt=10 warm-up at the first event -> script events.

use std::path::PathBuf;

use hokusai_core::tile::TILE_SIZE;
use hokusai_core::{BrushState, TiledSurface};
use hokusai_tile_mem::MemSurface;

#[derive(serde::Deserialize)]
struct Script {
    width: u32,
    height: u32,
    events: Vec<[f64; 4]>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 || args.len() > 5 {
        eprintln!("usage: {} brush.myb stroke.json out.bin [linear]", args[0]);
        std::process::exit(2);
    }
    let linear = args.get(4).map_or(true, |v| v != "0");
    let brush_text = std::fs::read_to_string(&args[1]).expect("read brush");
    let brush = hokusai_brush::from_str(&brush_text).expect("parse brush");
    let script_text = std::fs::read_to_string(&args[2]).expect("read script");
    let script: Script = serde_json::from_str(&script_text).expect("parse script");
    let width = script.width as usize;
    let height = script.height as usize;

    hokusai_core::brushmodes::reset_get_color_rng();
    let mut state = BrushState::default();
    let mut surface = MemSurface::new();
    surface.begin_atomic();

    if let Some(first) = script.events.first() {
        brush.stroke_to_full(
            &mut state,
            &mut surface,
            first[0] as f32,
            first[1] as f32,
            0.0,
            0.0,
            0.0,
            10.0,
            1.0,
            0.0,
            0.0,
            linear,
        );
    }
    for event in &script.events {
        brush.stroke_to_full(
            &mut state,
            &mut surface,
            event[0] as f32,
            event[1] as f32,
            event[2] as f32,
            0.0,
            0.0,
            event[3],
            1.0,
            0.0,
            0.0,
            linear,
        );
    }
    surface.end_atomic();

    let mut pixels = vec![0u16; width * height * 4];
    let tiles_x = (width + TILE_SIZE as usize - 1) / TILE_SIZE as usize;
    let tiles_y = (height + TILE_SIZE as usize - 1) / TILE_SIZE as usize;
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let Some(tile) = surface.tile(tx as i32, ty as i32) else {
                continue;
            };
            let x0 = tx * TILE_SIZE as usize;
            let y0 = ty * TILE_SIZE as usize;
            let w = (width - x0).min(TILE_SIZE);
            let h = (height - y0).min(TILE_SIZE);
            for y in 0..h {
                let src = &tile[y];
                let dst_start = ((y0 + y) * width + x0) * 4;
                for x in 0..w {
                    let dst = dst_start + x * 4;
                    pixels[dst..dst + 4].copy_from_slice(&src[x]);
                }
            }
        }
    }

    let mut out = Vec::with_capacity(8 + pixels.len() * 2);
    out.extend_from_slice(&(width as u32).to_le_bytes());
    out.extend_from_slice(&(height as u32).to_le_bytes());
    for value in pixels {
        out.extend_from_slice(&value.to_le_bytes());
    }
    let out_path = PathBuf::from(&args[3]);
    std::fs::write(&out_path, out).expect("write output");
    eprintln!("rendered {} events -> {}", script.events.len(), out_path.display());
}
