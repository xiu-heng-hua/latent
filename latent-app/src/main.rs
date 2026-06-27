//! `latent` — command-line entry point.

mod gui;

use std::error::Error;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
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

/// The output bit depth, chosen on the `develop` command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Depth {
    /// 8 bits per channel.
    #[value(name = "8")]
    Eight,
    /// 16 bits per channel (TIFF and PNG only).
    #[value(name = "16")]
    Sixteen,
}

impl From<Depth> for latent_export::Depth {
    fn from(d: Depth) -> Self {
        match d {
            Depth::Eight => latent_export::Depth::Eight,
            Depth::Sixteen => latent_export::Depth::Sixteen,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Develop a RAW file into an output image file.
    Develop {
        /// Input RAW file.
        input: PathBuf,
        /// Output image file (format chosen by extension, e.g. .jpg/.png/.tiff).
        output: PathBuf,
        /// Output bit depth (default: 16 for tif/tiff/png, 8 for jpg/jpeg).
        #[arg(long)]
        depth: Option<Depth>,
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
pub fn develop_to_image(input: &Path) -> Result<(ImageBuf, latent_raw::Metadata), Box<dyn Error>> {
    let raw = latent_raw::unpack(input)?;
    let mut mosaic = raw.normalized();
    raw.apply_white_balance(&mut mosaic);
    let mut camera_rgb = raw.demosaic_mhc(&mosaic);
    raw.reconstruct_highlights(&mut camera_rgb);
    let to_working = raw
        .color_matrix()
        .ok_or("camera color matrix is singular")?;
    Ok((color::apply_matrix(&camera_rgb, &to_working), raw.meta))
}

/// Develop `input` and encode it to `output`. With no explicit `depth`, the depth
/// is chosen by the output format (16-bit for tif/tiff/png, 8-bit for jpg/jpeg)
/// so a wide-gamut TIFF/PNG export is banding-free without a flag. An unsupported
/// extension is rejected by the encoder with a typed error.
fn develop(input: &Path, output: &Path, depth: Option<Depth>) -> Result<(), Box<dyn Error>> {
    let (img, _) = develop_to_image(input)?;
    latent_export::save_auto(&img, output, depth.map(Into::into))?;
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Develop {
            input,
            output,
            depth,
        } => develop(&input, &output, depth).map(|()| println!("wrote {}", output.display())),
        Command::Open { input, gpu } => gui::run(&input, select_backend(gpu)),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_develop() {
        let cli = Cli::try_parse_from(["latent", "develop", "in.raw", "out.tiff"])
            .expect("develop should parse");
        match cli.command {
            Command::Develop {
                input,
                output,
                depth,
            } => {
                assert_eq!(input, PathBuf::from("in.raw"));
                assert_eq!(output, PathBuf::from("out.tiff"));
                assert_eq!(depth, None);
            }
            _ => panic!("expected the develop subcommand"),
        }
    }

    #[test]
    fn develop_parses_depth_flag() {
        let cli = Cli::try_parse_from(["latent", "develop", "in.raw", "out.tiff", "--depth", "8"])
            .expect("--depth 8 should parse");
        match cli.command {
            Command::Develop { depth, .. } => assert_eq!(depth, Some(Depth::Eight)),
            _ => panic!("expected the develop subcommand"),
        }
        let cli = Cli::try_parse_from(["latent", "develop", "in.raw", "out.png", "--depth", "16"])
            .expect("--depth 16 should parse");
        match cli.command {
            Command::Develop { depth, .. } => assert_eq!(depth, Some(Depth::Sixteen)),
            _ => panic!("expected the develop subcommand"),
        }
        // A depth outside the accepted set is rejected by the parser.
        assert!(
            Cli::try_parse_from(["latent", "develop", "in.raw", "out.tiff", "--depth", "12"])
                .is_err()
        );
    }

    #[test]
    fn cli_parses_open_gpu() {
        let with = Cli::try_parse_from(["latent", "open", "in.raw", "--gpu"])
            .expect("open --gpu should parse");
        match with.command {
            Command::Open { input, gpu } => {
                assert_eq!(input, PathBuf::from("in.raw"));
                assert!(gpu);
            }
            _ => panic!("expected the open subcommand"),
        }
        let without = Cli::try_parse_from(["latent", "open", "in.raw"]).expect("open should parse");
        match without.command {
            Command::Open { gpu, .. } => assert!(!gpu),
            _ => panic!("expected the open subcommand"),
        }
    }

    #[test]
    fn cli_rejects_missing_output() {
        // `develop` requires both an input and an output path.
        assert!(Cli::try_parse_from(["latent", "develop", "in.raw"]).is_err());
    }

    #[test]
    fn develop_errors_on_bad_input() {
        // A nonexistent/garbage input fails at unpack and surfaces as an `Err`
        // (which `main` maps to a non-zero exit), not a panic.
        let bad = std::env::temp_dir().join("latent_develop_bad_input_test.raw");
        std::fs::write(&bad, b"not a raw file").unwrap();
        let out = std::env::temp_dir().join("latent_develop_bad_input_out.tiff");
        std::fs::remove_file(&out).ok();

        assert!(develop(&bad, &out, None).is_err());
        assert!(!out.exists(), "no output should be written for a bad input");

        std::fs::remove_file(&bad).ok();
    }

    #[test]
    fn develop_selects_16bit_for_tiff_and_8bit_for_jpeg() {
        // The depth routing lives in `save_auto`; develop just forwards the flag.
        // Drive the encoder directly through a developed-image stand-in so the
        // routing is checked without a real RAW decode.
        let mut img = ImageBuf::new(2, 1);
        img.set(0, 0, [0.0, 0.0, 0.0]);
        img.set(1, 0, [0.5, 0.5, 0.5]);

        let tiff = std::env::temp_dir().join("latent_develop_route_tiff.tiff");
        latent_export::save_auto(&img, &tiff, None).expect("tiff");
        assert!(matches!(
            image::open(&tiff).unwrap().color(),
            image::ColorType::Rgb16
        ));
        std::fs::remove_file(&tiff).ok();

        let jpg = std::env::temp_dir().join("latent_develop_route_jpg.jpg");
        latent_export::save_auto(&img, &jpg, None).expect("jpg");
        assert!(matches!(
            image::open(&jpg).unwrap().color(),
            image::ColorType::Rgb8
        ));
        std::fs::remove_file(&jpg).ok();

        // 16-bit JPEG has no encoding path; forcing it is rejected, not written.
        let jpg16 = std::env::temp_dir().join("latent_develop_route_jpg16.jpg");
        std::fs::remove_file(&jpg16).ok();
        assert!(
            latent_export::save_auto(&img, &jpg16, Some(latent_export::Depth::Sixteen)).is_err()
        );
    }
}
