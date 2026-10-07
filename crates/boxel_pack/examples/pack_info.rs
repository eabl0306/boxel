//! Summarize a Minecraft resource pack on disk.
//!
//! ```text
//! cargo run -p boxel_pack --example pack_info -- path/to/pack
//! ```
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

use std::env;
use std::process::ExitCode;
use std::time::Instant;

use boxel_pack::ResourcePack;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: pack_info <pack-dir>");
        return ExitCode::from(1);
    };
    if args.next().is_some() {
        eprintln!("usage: pack_info <pack-dir>");
        return ExitCode::from(1);
    }

    let start = Instant::now();
    let pack = match ResourcePack::open(path.as_ref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let elapsed = start.elapsed();

    let meta = pack.meta();
    println!("pack: {}", meta.root.display());
    println!("description: {}", meta.description.replace('\n', " / "));
    println!(
        "format: effective {}, range {}..={}, pack_format {:?}",
        pack.format(),
        meta.format_range.min,
        meta.format_range.max,
        meta.pack_format
    );
    println!("overlays: {}", meta.overlays.len());
    println!("textures: {}", pack.texture_count());
    println!("models: {}", pack.model_count());
    println!("blockstates: {}", pack.blockstate_count());
    println!("warnings: {}", pack.warnings().len());
    println!("elapsed: {:.3}s", elapsed.as_secs_f64());
    for w in pack.warnings().iter().take(20) {
        println!("  warn[{}]: {}", w.kind, w.message);
    }
    if pack.warnings().len() > 20 {
        println!("  … {} more", pack.warnings().len() - 20);
    }
    ExitCode::SUCCESS
}
