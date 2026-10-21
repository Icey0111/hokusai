//! Debug aid: dump BrushState fields after each event in cref/ref_runner.c's format.

use hokusai_core::state::BrushStateField;
use hokusai_core::{BrushState, TiledSurface};
use hokusai_tile_mem::MemSurface;

#[derive(serde::Deserialize)]
struct Script {
    width: u32,
    height: u32,
    events: Vec<[f64; 4]>,
}

fn trace(label: &str, state: &BrushState) {
    print!("{label}");
    for field in BrushStateField::ALL {
        print!(" {:08x}", state.get_state(field).to_bits());
    }
    println!();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: {} brush.myb stroke.json [linear]", args[0]);
        std::process::exit(2);
    }
    let linear = args.get(3).map_or(true, |v| v != "0");
    let text = std::fs::read_to_string(&args[1]).expect("read brush");
    let brush = hokusai_brush::from_str(&text).expect("parse brush");
    let script: Script =
        serde_json::from_str(&std::fs::read_to_string(&args[2]).expect("read script"))
            .expect("parse script");
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
        trace("W", &state);
    }
    for (i, event) in script.events.iter().enumerate() {
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
        trace(&format!("E{i}"), &state);
    }
    let _ = script.width;
    let _ = script.height;
}
