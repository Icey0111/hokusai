//! Dump the effective brush settings/mappings in the same text format as cref/dump_brush.c.

use hokusai_core::BrushSetting;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: {} brush.myb", args[0]);
        std::process::exit(2);
    }
    let text = std::fs::read_to_string(&args[1]).expect("read brush");
    let brush = hokusai_brush::from_str(&text).expect("parse brush");
    for setting in BrushSetting::ALL {
        let sv = brush.get(*setting);
        println!("B {} {:.9}", setting.cname(), sv.base_value);
        for mapping in &sv.inputs {
            print!(
                "M {} {} {}",
                setting.cname(),
                mapping.input.cname(),
                mapping.points.len()
            );
            for (x, y) in &mapping.points {
                print!(" {:.9} {:.9}", x, y);
            }
            println!();
        }
    }
}
