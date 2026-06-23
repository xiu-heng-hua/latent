//! `latent` — command-line entry point.

mod gui;

use std::error::Error;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use latent_cpu::CpuBackend;
use latent_image::ImageBuf;
use latent_image::color;
use latent_pipeline::Backend;

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
    /// Open a RAW file in the editor window.
    Open {
        /// Input RAW file.
        input: PathBuf,
        /// Render on the GPU if a device is available (falls back to the CPU).
        #[arg(long)]
        gpu: bool,
    },
}

/// Pick a rendering backend at the application's composition root. With `--gpu`,
/// try the GPU backend and fall back to the CPU one if no device is available;
/// otherwise use the CPU backend (the complete, always-available reference).
fn select_backend(use_gpu: bool) -> Box<dyn Backend> {
    if use_gpu {
        match latent_gpu::GpuBackend::new() {
            Ok(gpu) => {
                eprintln!("using GPU backend");
                return Box::new(gpu);
            }
            Err(e) => eprintln!("GPU unavailable ({e}); using CPU backend"),
        }
    }
    Box::new(CpuBackend)
}

/// Decode and develop a RAW into a linear working image in SOURCE coordinates:
/// normalize, white balance, demosaic, reconstruct blown highlights, then the
/// camera→working color transform. This is the base the pipeline renders
/// adjustments and geometry over.
pub fn develop_to_image(input: &Path) -> Result<ImageBuf, Box<dyn Error>> {
    let raw = latent_raw::unpack(input)?;
    let mut mosaic = raw.normalized();
    raw.apply_white_balance(&mut mosaic);
    let mut camera_rgb = raw.demosaic_mhc(&mosaic);
    raw.reconstruct_highlights(&mut camera_rgb);
    let to_working = raw
        .color_matrix()
        .ok_or("camera color matrix is singular")?;
    Ok(color::apply_matrix(&camera_rgb, &to_working))
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
        Command::Open { input, gpu } => gui::run(&input, select_backend(gpu)),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
