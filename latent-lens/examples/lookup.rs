//! Manual check of the live lensfun lookup — the database is external (like
//! real-RAW decode), so this is a runnable demo rather than a committed test.
//!
//! Usage: `cargo run -p latent-lens --example lookup [maker model lens focal]`

use latent_lens::{Database, version};

fn main() {
    let (a, b, c, d) = version();
    println!("linked lensfun {a}.{b}.{c}.{d}");

    let args: Vec<String> = std::env::args().skip(1).collect();
    let maker = args.first().map_or("Samsung", String::as_str);
    let model = args.get(1).map_or("NX100", String::as_str);
    let lens = args
        .get(2)
        .map_or("Samsung NX 30mm f/2 Pancake", String::as_str);
    let focal: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30.0);

    let db = Database::load().expect("load the lensfun database (install liblensfun-data)");
    match db.find_profile(maker, model, lens, focal, 8.0, 1000.0) {
        Some(p) => println!(
            "{lens} @ {focal}mm:\n  center      = {:?}\n  distortion  = {:?}\n  ca          = {:?}\n  vignetting  = {:?}",
            p.center, p.distortion, p.ca, p.vignetting
        ),
        None => println!("no profile found for {maker} / {model} / {lens}"),
    }
}
