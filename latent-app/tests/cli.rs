//! End-to-end checks of the `latent` binary's exit-code contract: a failed
//! develop must exit non-zero and report the error on stderr, the path the
//! shell and scripts rely on. The binary path is injected by Cargo.

use std::process::Command;

/// A develop of a garbage input fails at unpack; the process must exit non-zero
/// and print an `error:` line to stderr (not panic, not exit 0).
#[test]
fn cli_exits_nonzero_on_error() {
    let bad = std::env::temp_dir().join("latent_cli_exit_bad_input.raw");
    std::fs::write(&bad, b"not a raw file").unwrap();
    let out = std::env::temp_dir().join("latent_cli_exit_out.tiff");
    std::fs::remove_file(&out).ok();

    let output = Command::new(env!("CARGO_BIN_EXE_latent"))
        .args(["develop"])
        .arg(&bad)
        .arg(&out)
        .output()
        .expect("run the latent binary");

    assert!(!output.status.success(), "a bad develop must exit non-zero");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("error:"),
        "stderr should carry an error: {stderr}"
    );
    assert!(!out.exists(), "no output should be written for a bad input");

    std::fs::remove_file(&bad).ok();
}
