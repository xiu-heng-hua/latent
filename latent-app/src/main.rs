//! `latent` — command-line entry point.

use std::error::Error;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use latent_image::ImageBuf;
use latent_image::color;

/// A small, readable RAW developer.
#[derive(Parser)]
#[command(name = "latent", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Develop a RAW file into an output image file.
    Develop {
        /// Input RAW file.
        input: PathBuf,
        /// Output image file (format chosen by extension, e.g. .jpg/.png/.tiff).
        output: PathBuf,
    },
}

/// Decode and develop a RAW into a linear-sRGB working image (before the final
/// gamma encode): normalize, white balance, demosaic, then the camera→working
/// color transform.
fn develop_to_image(input: &Path) -> Result<ImageBuf, Box<dyn Error>> {
    let raw = latent_raw::unpack(input)?;
    let mut mosaic = raw.normalized();
    raw.apply_white_balance(&mut mosaic);
    let camera_rgb = raw.demosaic_mhc(&mosaic);
    let to_srgb = raw
        .color_matrix()
        .ok_or("camera color matrix is singular")?;
    Ok(color::apply_matrix(&camera_rgb, &to_srgb))
}

fn develop(input: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    let img = develop_to_image(input)?;
    latent_export::save(&img, output)?;
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Develop { input, output } => {
            develop(&input, &output).map(|()| println!("wrote {}", output.display()))
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
