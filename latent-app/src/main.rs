//! `latent` — command-line entry point.

mod gui;

use std::error::Error;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use latent_image::ImageBuf;
use latent_image::{Orientation, color};

/// A small, readable RAW developer.
///
/// A bare path (`latent photo.nef`) is shorthand for `latent open photo.nef`; with
/// no arguments at all the editor opens on its welcome screen. The `open` and
/// `develop` subcommands are unchanged.
#[derive(Parser)]
#[command(name = "latent", version, about)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// A RAW file to open in the editor (shorthand for `open <input>`). Ignored
    /// when a subcommand is given; absent with no subcommand launches the welcome
    /// state.
    input: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
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

/// Decode and develop a RAW into a linear working image: normalize, white
/// balance, demosaic, reconstruct blown highlights, the camera→working color
/// transform, and finally the display orientation. This is the base the pipeline
/// renders adjustments and geometry over.
///
/// Orientation is applied **last**, after the color matrix: it is display
/// geometry, not sensor processing (rotating before demosaic would break the CFA
/// phase, and before the color matrix would scramble per-sensor-channel math), so
/// every coordinate downstream — the texture, the fit, brush/mask normalized
/// coords, crop — is already in upright display space. It permutes pixels only,
/// never changing a value. A later manual rotate/flip composes *on top of* this
/// already-upright base in the pipeline's geometry stage; it must not re-read the
/// raw `flip` here.
pub fn develop_to_image(input: &Path) -> Result<(ImageBuf, latent_raw::Metadata), Box<dyn Error>> {
    let raw = latent_raw::unpack(input)?;
    let mut mosaic = raw.normalized();
    raw.apply_white_balance(&mut mosaic);
    let mut camera_rgb = raw.demosaic_mhc(&mosaic);
    raw.reconstruct_highlights(&mut camera_rgb);
    let to_working = raw
        .color_matrix()
        .ok_or("camera color matrix is singular")?;
    let working = color::apply_matrix(&camera_rgb, &to_working);
    let upright = working.oriented(Orientation::from_libraw(raw.meta.orientation));
    Ok((upright, raw.meta))
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
        Some(Command::Develop {
            input,
            output,
            depth,
        }) => develop(&input, &output, depth).map(|()| println!("wrote {}", output.display())),
        // The `open` subcommand's explicit `--gpu` wins over the persisted pref.
        Some(Command::Open { input, gpu }) => open_editor(Some(input.as_path()), Some(gpu)),
        // No subcommand: a bare path opens that file, nothing at all opens the
        // welcome state. With no `--gpu` flag, both honor the persisted pref.
        None => open_editor(cli.input.as_deref(), None),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Open the editor window on `input` (or the welcome state for `None`), loading
/// the persisted app config. `gpu` forces the backend when `Some`; `None` honors
/// the config's persisted GPU preference. The single entry point the `open`
/// subcommand and the bare-path/no-args paths share.
fn open_editor(input: Option<&Path>, gpu: Option<bool>) -> Result<(), Box<dyn Error>> {
    let config = gui::config_load();
    let use_gpu = gpu.unwrap_or(config.gpu);
    let (backend, kind) = gui::select_backend(use_gpu);
    gui::run(input, backend, kind, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_develop() {
        let cli = Cli::try_parse_from(["latent", "develop", "in.raw", "out.tiff"])
            .expect("develop should parse");
        match cli.command {
            Some(Command::Develop {
                input,
                output,
                depth,
            }) => {
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
            Some(Command::Develop { depth, .. }) => assert_eq!(depth, Some(Depth::Eight)),
            _ => panic!("expected the develop subcommand"),
        }
        let cli = Cli::try_parse_from(["latent", "develop", "in.raw", "out.png", "--depth", "16"])
            .expect("--depth 16 should parse");
        match cli.command {
            Some(Command::Develop { depth, .. }) => assert_eq!(depth, Some(Depth::Sixteen)),
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
            Some(Command::Open { input, gpu }) => {
                assert_eq!(input, PathBuf::from("in.raw"));
                assert!(gpu);
            }
            _ => panic!("expected the open subcommand"),
        }
        let without = Cli::try_parse_from(["latent", "open", "in.raw"]).expect("open should parse");
        match without.command {
            Some(Command::Open { gpu, .. }) => assert!(!gpu),
            _ => panic!("expected the open subcommand"),
        }
    }

    #[test]
    fn cli_parses_bare_path_as_open() {
        // A lone path (no subcommand) is shorthand for `open <input>`: it parses to
        // the top-level `input` with no subcommand.
        let cli = Cli::try_parse_from(["latent", "photo.nef"]).expect("bare path should parse");
        assert!(cli.command.is_none(), "a bare path is not a subcommand");
        assert_eq!(cli.input, Some(PathBuf::from("photo.nef")));
    }

    #[test]
    fn cli_no_args_is_welcome() {
        // No arguments at all: no subcommand and no input — the welcome state.
        let cli = Cli::try_parse_from(["latent"]).expect("no args should parse");
        assert!(cli.command.is_none());
        assert_eq!(cli.input, None);
    }

    #[test]
    fn cli_subcommand_wins_over_bare_path() {
        // `open`/`develop` still parse exactly as before; the bare positional does
        // not swallow a real subcommand (args conflict with subcommands).
        let cli = Cli::try_parse_from(["latent", "open", "in.raw"]).expect("open parses");
        assert!(matches!(cli.command, Some(Command::Open { .. })));
        assert_eq!(cli.input, None);
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
    fn orientation_seam_makes_a_portrait_upright() {
        // `develop_to_image` applies orientation as its last step, decoding the
        // LibRaw `flip` code into the geometry transform. A real RAW fixture is
        // impractical here, so this pins the seam the develop wiring is a
        // one-liner over: a landscape working image stays landscape for an
        // upright code, and a `flip == 6` (90° CW) shot comes out with portrait
        // dimensions (height > width).
        let landscape = ImageBuf::new(4, 2);
        let upright = landscape.oriented(Orientation::from_libraw(0));
        assert_eq!((upright.width(), upright.height()), (4, 2));
        let portrait = landscape.oriented(Orientation::from_libraw(6));
        assert!(
            portrait.height() > portrait.width(),
            "a flip==6 shot must come out portrait: {}x{}",
            portrait.width(),
            portrait.height()
        );
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
