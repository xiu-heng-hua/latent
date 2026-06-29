# Code Review — Export, Lens (FFI), and Application/UI crates

Scope: `latent-export`, `latent-lens` (+ `build.rs`, `examples/lookup.rs`), and
`latent-app` (`main.rs` CLI + `gui.rs` egui editor). Engineering focus:
correctness, FFI/unsafe soundness, file I/O & error handling, UI state, CLI
design, edge cases, tests. Algorithm theory is audited by a separate team; where
a finding straddles theory and wiring I flag the engineering part only.

Environment notes for reproducibility:
- `pkg-config --exists lensfun` → **found** (`/usr/include/lensfun/lensfun.h`),
  so `latent-lens` builds and its FFI is exercised at compile/link time.
- `cargo clippy -p latent-export -p latent-lens -p latent-app --all-targets` →
  **clean** (exit 0).
- `cargo test -p latent-export -p latent-lens -p latent-app` → **all pass**
  (export 8, lens 9, app 1; lens version test links a real lib; the live DB
  lookup is a manual example, not a test).
- `cargo run -p latent-app -- --help` / `develop --help` → help renders; clap
  wiring is correct.
- The egui GUI cannot be launched headless in this environment; the `gui.rs`
  review is by code reading plus the one unit test it carries.

---

## 1. Overview & overall quality per crate

**`latent-export` — excellent.** A single shared output transform (`to_display`)
feeds both 8- and 16-bit encoders and the GUI preview, so preview/file agreement
is structural, not coincidental. ICC embedding is per-format and tested for PNG
and TIFF. Quantization clamps before rounding. Tests pin exact code values
(254/255/188, 16-bit ≈48196) so a silent drift in the transform fails the
suite. The only real gaps are an **untagged silent fallback** for unknown
extensions and a couple of `.expect()`s that are technically reachable via the
public API (see findings).

**`latent-lens` — very good, with one important unit/semantics caveat.** The FFI
wrapper is small, RAII-clean, and careful: null checks on every returned
pointer, `lf_free` on both the camera and lens lists on every path (including
early returns), `lf_db_destroy` in `Drop`, `CString` lifetimes that outlive the
calls, and a `.is_finite()` guard against divide-by-near-zero in the
coefficient mapping. Unsafe blocks are correctly scoped with accurate SAFETY
comments. The model→profile mapping is well unit-tested. The one substantive
concern is a **normalization mismatch on the optical-center mapping** (lensfun
normalizes the center shift by the *max* image dimension; `LensProfile.center`
is documented as frame-relative), plus minor robustness gaps (coefficients are
not range-validated, only finiteness-checked; thread-safety of the handle is
undocumented).

**`latent-app` — good.** The CLI is clean and idiomatic clap with correct
non-zero exit on error. The GUI is well structured: one `History` per variant as
the single source of truth, gesture-based undo grouping, autosave gated on
gesture completion, preview that reuses the export transform. The two real
engineering risks are (a) **all rendering runs on the UI thread** — a
full-resolution export (and even a heavy preview render) blocks the event loop —
and (b) a handful of **`.unwrap()`s on `self.texture`** that are safe today only
by an invariant that isn't enforced by the type system. No user *input* path
panics (bad file / unsupported RAW / missing sidecar all degrade gracefully).

---

## 2. Findings by severity

### High

**H1. GUI renders synchronously on the UI thread — full-res export and heavy
previews freeze the window.**
`latent-app/src/gui.rs:173-183` (`export`) and `:156-170` (`render_preview`),
called from `update` at `:335-337` and `:390-392`.
`render(&self.full, …)` runs the entire pipeline at full resolution on the
calling thread, which is the egui update thread. While it runs, the window does
not repaint or process input — on a large RAW the editor appears hung for the
duration of an export. Preview re-render is bounded by `PREVIEW_MAX_DIM = 1600`
so it's cheaper, but a slow backend or a heavy local-adjustment stack can still
stutter the UI because it too runs inline in `update`.
- **Impact:** Poor responsiveness; the app looks frozen during export. Not a
  crash, but the most user-visible engineering weakness.
- **Fix:** Move render/export onto a worker thread (`std::thread` +
  channel, or `poll_promise`/`std::sync::mpsc`), keep a "rendering…" status,
  and `ctx.request_repaint()` on completion. At minimum, run `export` off-thread
  since it is the worst case (full resolution).

**H2. Unknown output extension silently writes an untagged file (no ICC, and via
a different code path).**
`latent-export/src/lib.rs:154` — the `_ => buf.save(path)` arm.
For any extension that isn't png/tif/tiff/jpg/jpeg (e.g. `.bmp`, `.webp`, or a
typo like `.jpe`/`.png ` / no extension), the file is written with **no ICC
profile**, so the color is untagged and will be misinterpreted by color-managed
viewers. There is no warning to the user. The doc comment acknowledges this
("an unknown extension falls back to an untagged save"), so it's intentional,
but it's silent and reachable directly from the CLI (`latent develop in.raw
out.bmp`) and from the GUI export path field.
- **Impact:** Silent color-management loss on a class of valid outputs; user has
  no signal. Worse for a photo app where color fidelity is the point.
- **Fix:** Either (a) reject unknown/unsupported extensions with a clear error
  (`ImageError::Unsupported` with an actionable message), or (b) keep the
  fallback but log/return a warning so it isn't silent. Option (a) is cleaner
  given the crate's stated contract that "an sRGB ICC profile is embedded."

### Medium

**M1. `save()` / `save_16()` do not validate dimensions; a 0×N image takes the
untagged path or writes an empty file rather than erroring.**
`latent-export/src/lib.rs:86-90`, `95-106`.
`RgbImage::from_raw(w, h, bytes)` with `w==0||h==0` and an empty `bytes`
succeeds (length 0 matches `0` pixels), so the `.expect(...)` at `:88` does *not*
fire — instead a zero-dimension image is handed to the encoder. Most encoders
either error or write a degenerate file; the behavior is format-dependent and
unvalidated. There is no explicit "empty image" guard.
- **Impact:** Low-frequency but undefined behavior for empty/degenerate images;
  inconsistent across formats.
- **Fix:** Early-return a typed error when `img.width()==0 || img.height()==0`.

**M2. `save()`'s `.expect("buffer length matches the image dimensions")` is
reachable in principle via integer overflow on huge dimensions.**
`latent-export/src/lib.rs:87-88`.
`to_srgb8` builds a `Vec<u8>` of length `width*height*3`. For pathological
dimensions this multiplication can overflow `usize` semantics inside `image`'s
`from_raw` length check and return `None`, tripping the `expect`. In practice
RAW files won't reach this, and `Vec::with_capacity(img.len()*3)` would OOM
first, so this is theoretical — but it's a panic in a library function on a
public API rather than a returned `Err`.
- **Impact:** Theoretical panic; not reachable from normal RAW sizes.
- **Fix:** Replace the `expect` with a mapped `ImageError` (e.g.
  `Unsupported`/`Parameter`) so the function honors its `ImageResult` contract
  for all inputs.

**M3. Optical-center normalization mismatch between lensfun and `LensProfile`.**
`latent-lens/src/lib.rs:142`: `center = [0.5 + (*lens).CenterX, 0.5 + (*lens).CenterY]`.
The lensfun header (lfLens::CenterX, lines 818-828) documents the shift as
normalized so that **`1.0` = the maximal image dimension** (the same divisor for
both X and Y, because the lens projects a circle). `LensProfile.center` is
documented (`latent-edit/src/lib.rs`) as "normalized to the frame, where
`(0.5,0.5)` is the image center" — i.e. per-axis frame-relative. Adding `0.5`
directly assumes lensfun's X/Y are already frame-relative half-units, which is
only true on a square sensor. On a non-square frame the off-center shift is
scaled wrong on the longer axis.
- **Impact:** For the overwhelmingly common centered lens (CenterX/Y = 0) this is
  exactly correct, so it's invisible in practice; it only bites the rare
  off-center calibration. The *theory* of the right divisor is the algorithm
  team's call, but the **engineering wiring** here silently assumes square
  normalization and that assumption isn't documented or guarded.
- **Fix:** Confirm `LensProfile.center`'s intended normalization with the
  pipeline, then map explicitly (likely scale CenterX/Y by
  `max_dim/width`, `max_dim/height` respectively before the `0.5 +`). Add a
  comment pinning the convention either way.

**M4. Extracted lens coefficients are finiteness-checked but not range- or
NaN-from-source validated for TCA/vignetting.**
`latent-lens/src/lib.rs:212-218` (`ca_offsets`), `225-231` (`vignetting_falloff`).
`radial_distortion` (`:198`) guards non-finite output, but `ca_offsets` and
`vignetting_falloff` pass interpolated terms straight through with no finiteness
or sanity check. If lensfun ever returned a NaN/garbage term (corrupt DB entry),
it would flow into the engine unchecked. Distortion is guarded; the other two
are not, which is inconsistent.
- **Impact:** Low (lensfun data is generally clean), but an inconsistency in the
  defensive posture: one of three mappers is hardened, two are not.
- **Fix:** Apply the same `is_finite()` (and optionally a plausibility clamp)
  guard to the CA and vignetting outputs.

### Low

**L1. JPEG quality is not configurable (encoder default only).**
`latent-export/src/lib.rs:148-153`. `JpegEncoder::new(file)` uses image-rs's
default quality (75). For an export-quality photo developer this is low and not
surfaceable from CLI or GUI. Add a quality parameter (or use a higher default,
e.g. 92, via `new_with_quality`).

**L2. `develop` ignores the format-vs-bit-depth choice; CLI always writes 8-bit.**
`latent-app/src/main.rs:73-77` always calls `latent_export::save` (8-bit), never
`save_16`, even for a `.tiff` output where 16-bit is the whole point of having
`save_16`. The 16-bit path exists and is tested but is unreachable from the CLI;
only the export function in the GUI also uses 8-bit (`gui.rs:179`). Consider a
`--depth 8|16` flag (or pick 16 automatically for tiff/png).

**L3. GUI `self.texture.unwrap()` relies on an unenforced invariant.**
`latent-app/src/gui.rs:339-340` unwraps `self.texture` after the `if dirty`
render block. It is sound today because the first frame always has
`dirty = self.texture.is_none()` true (`:188`) and `render_preview` always sets
the texture — but that coupling is implicit. If a future edit reorders the
`dirty` computation or makes `render_preview` fallible, this becomes a panic on
the first frame. Prefer threading the texture handle out of `render_preview`'s
return, or `let-else`/`expect` with a message that documents the invariant.

**L4. Export path is a free-text field with no extension validation in the GUI.**
`latent-app/src/gui.rs:312-318`. The user types an arbitrary output path; an
unknown extension hits the silent untagged fallback (H2) and an unwritable path
surfaces only as a status string. Acceptable, but pairs badly with H2.

**L5. `auto_lens_profile` loads the whole lensfun DB on every editor open, on the
UI-blocking path.**
`latent-app/src/gui.rs:81-94` is called from `run` before the window opens, so
it doesn't block an interactive frame, but `Database::load()` reads the full
on-disk DB synchronously every open. Fine for now; if open latency matters,
cache or lazy-load.

### Nit

**N1.** `examples/lookup.rs:20` uses `.expect("load the lensfun database …")`. As
a manual demo this is acceptable, but it will panic (not print a friendly line)
when the DB package is absent — slightly at odds with the crate's otherwise
graceful "returns None when no DB" posture in `find_profile`.

**N2.** `latent-export/src/lib.rs:111-115` `srgb_icc()` `.expect("encode sRGB ICC
profile")` on `moxcms` encoding. This is a pure, input-independent call that
cannot realistically fail, so the expect is fine; noting it only for the audit
trail of panics-in-library-code.

**N3.** `gui.rs:885` and `:908` clamp `*sel` defensively after list mutations —
good — but the same clamp is duplicated; a small helper would DRY it.

---

## 3. Lens FFI / unsafe soundness assessment

Overall: **sound.** Each `unsafe` block is narrowly scoped and the SAFETY
comments accurately state the invariant being relied on.

- **Allocation & null handling:** `lf_db_new` result is null-checked
  (`:56-59`); `find_profile` null-checks both the list pointer and its first
  element for cameras (`:96`) and lenses (`:106`) before dereferencing.
- **Memory management / leaks:** `lf_free` is called on the camera list on every
  exit path (early-return at `:98`, and the normal path at `:105`) and on the
  lens list likewise (`:108`, `:113`). The list elements are owned by the DB
  (correct — only the array is freed, not the entries). `Drop` calls
  `lf_db_destroy` exactly once (`:119-124`). No leak found; no double-free found.
- **Lifetimes:** The `lfLens` pointer passed to `lens_to_profile` is used and
  fully consumed *before* the corresponding `lf_free(lenses)` (`:112-113`), so
  it never outlives the DB-owned data. The three `CString`s are bound to locals
  that outlive the FFI calls (`:88-90`). Correct.
- **String handling:** `CString::new(...).ok()?` correctly turns an interior-NUL
  input into `None` rather than UB. Returned C strings are not read back here, so
  no `CStr::from_ptr` lifetime concerns.
- **Out-params:** `std::mem::zeroed()` for the calib structs is acceptable
  because they are plain-old-data C structs filled by the interpolate calls; the
  `!= 0` return is checked before the struct is read (`:146-159`). Good.
- **Missing DB / unknown lens:** Degrades gracefully — `load()` returns a typed
  `Error` (NoDatabase/WrongFormat/Alloc), and `find_profile` returns `None` for
  an unknown camera or lens. The GUI consumes both as "no profile" (`gui.rs:85`)
  and continues. No panic on the absent-DB or unknown-lens paths.
- **Thread-safety:** `Database` holds a raw `*mut lfDatabase` and is **not**
  `Send`/`Sync` (raw pointers aren't), which conservatively prevents sharing it
  across threads — good, since lensfun's DB handle is not documented thread-safe.
  Worth a one-line doc note that the handle is single-threaded by design,
  especially if H1's threading work later wants to call lensfun off-thread.

Caveats (not soundness, but robustness): the optical-center normalization (M3)
and the un-guarded CA/vignetting coefficients (M4).

---

## 4. UI responsiveness & preview-consistency assessment

**Preview consistency: excellent and verified.** `to_color_image` (`gui.rs:1051`)
calls `latent_export::to_srgb8`, the exact transform the saved file uses, and a
unit test (`gui.rs:1060-1076`) pins the neutral values (0→0, 0.5→188, 1.0→254)
to the same constants the export tests pin. Preview-matches-file is structural.

**Re-render is gated, not per-frame.** `update` computes `dirty` from real
triggers (first frame, variant switch, any slider `changed`, undo/redo,
painting) and only calls `render_preview` when `dirty` (`:335-337`), plus once
more after a brush stroke (`:390-392`). It does **not** recompute every frame —
good. Texture is updated via `tex.set` when it already exists (`:164-167`),
avoiding per-frame texture allocation.

**Responsiveness: the weak point (H1).** Both `render_preview` and `export` run
synchronously on the egui update thread. Preview is size-capped so usually
acceptable; export at full resolution is the worst case and will visibly freeze
the window. No worker thread, no progress, no cancellation. This is the single
most impactful engineering improvement for the GUI.

**Slider clamp ranges — all bounded; no unclamped value reaches the model.**
Every slider uses a bounded `egui::Slider` range, so out-of-range values cannot
be entered:
- Contrast `-1.0..=1.0` (`gui.rs:569`) — the prompt's "contrast > 1
  tone-inversion" case **cannot** be reached from the UI; the slider hard-clamps
  to ±1. (A hand-edited `.ron` sidecar could still carry an out-of-range value;
  the UI is not the only writer — see Test gaps.)
- Highlights/Shadows/Blacks `-1..=1`, WB temp/tint `-1..=1`, Saturation
  `0..=2`, Exposure `-5..=5`, Dehaze `0..=1`, Sharpen amount `0..=2`/radius
  `1..=10`, Clarity `-1..=1`/radius `5..=100`, NR `0..=0.3`, straighten
  `-45..=45`, vignette `-1..=1`, keystone `-0.8..=0.8`, crop `0..=1`, brush
  size `0.01..=0.5`/feather `0..=0.5`. All bounded and sensible.

**Undo/redo wiring — correct.** One `History` per variant; gesture grouping via
`gesture()` (`:401-408`) gives one undo step per drag, and `commit` records only
on real change (history.rs `:49-56`), so a drag-and-return creates no step.
Keyboard (Cmd/Ctrl+Z / Shift+Z / Y) and toolbar buttons both route through the
same `undo()/redo()`. Brush strokes are one undo step per stroke
(`begin` on press, `commit` on release, `:357/:382`). Variant switch and add are
handled; `local_sel` is clamped after deletes (`:885`,`:908`). Solid.

**Autosave — correct and debounced.** Gated on `is_idle()` (no gesture pending,
`:135`) and on a real diff against `saved` (`:139`), so it writes once per
completed edit, never mid-drag, and reports serialize/write failures into the
status line rather than panicking (`:146-152`).

---

## 5. Error-handling & robustness assessment

**CLI (`main.rs`): correct.** All commands return `Result<_, Box<dyn Error>>`;
`main` prints `error: {e}` to stderr and `exit(1)` on failure (`:87-90`), `0` on
success. Errors are propagated with `?` and the one custom error ("camera color
matrix is singular", `:69`) is a clear `&str`. The success message goes to
stdout. No `unwrap`/`expect` on user-input paths in the CLI.

**Export (`lib.rs`): mostly typed, two library panics.** Returns
`image::ImageResult`; file-create and encode errors propagate. The two `expect`s
(`save` buffer-length `:88`, `srgb_icc` `:114`) are library panics rather than
returned errors — practically unreachable (M2, N2) but a contract wrinkle for an
`ImageResult`-returning API. No dimension validation (M1). The untagged silent
fallback (H2) is the main error-*surfacing* gap: a real loss is not reported.

**Lens (`lib.rs`): well-typed and graceful.** `Error` enum distinguishes
Alloc/NoDatabase/WrongFormat; `load` maps the C error code to it; `find_profile`
returns `Option` for not-found. Nothing is unwrapped on the live path. The
example's `expect` (N1) is the only panic, and it's a demo.

**GUI (`gui.rs`): no panic reachable from bad user input.** Open a bad/unsupported
RAW → `develop_to_image` returns `Err`, surfaced by `run`'s `Result` and the
CLI's exit-1, not a panic. Missing sidecar → `read_to_string().ok()` → `None` →
default document (`:31-36`). Corrupt sidecar → `from_ron().ok()` → `None` →
default (so a malformed `.ron` is silently ignored — arguably should warn, but
it never crashes). Export to an unwritable path → status string, not a panic.
The only `unwrap`s in the GUI are the two texture unwraps (L3), sound by the
first-frame invariant, and the `min_by(...).unwrap()` over the fixed 5-point
curve range (`:511`) which is over a non-empty constant range (infallible).

Swallowed-error spots worth a note: corrupt-sidecar is silently dropped
(`:34`); unknown-extension export is silently untagged (H2). Neither crashes;
both could warn.

---

## 6. Test-coverage gaps

What exists is good and meaningful (exact-value pins, ICC round-trips, the
preview-equals-export test, the lens model-mapping tests with degenerate-input
guards). Gaps:

- **Export dimension/edge cases (none):** no test for a 0×0 or 0×N image
  (M1), nor for the **unknown-extension fallback** behavior (H2) — a test
  asserting either an error or a documented untagged write would lock the
  contract. No test that a `.jpg` carries ICC (only png/tiff are covered),
  though the code path is the same.
- **Lens lookup (no automated coverage of the live query):** by design the DB is
  external, but the `Database`→`find_profile` plumbing (null/empty-list handling,
  free-on-every-path, not-found→`None`) is **untested**. A stub/fake lensfun (or
  a feature-gated test that runs only when `liblensfun-data` is present) would
  catch a regression in the pointer/free logic that the model-mapping unit tests
  cannot. The `lens_to_profile` aggregation (which fields land where) is also
  untested end-to-end; only the three pure mappers are.
- **CLI arg parsing (none):** clap derive is exercised only by `--help` manually.
  A `Cli::try_parse_from([...])` test per subcommand (and an error case, e.g.
  missing OUTPUT) is cheap and would pin the interface, including the `--gpu`
  flag.
- **`develop`/`develop_to_image` error path (none):** the "singular color
  matrix" error and the unpack-failure path have no test; a fixture or a
  failure-injecting stub would cover exit-code behavior.
- **GUI state transitions (only one test):** `to_color_image` is tested. Not
  tested: the `gesture()` begin/commit/discrete logic, autosave's
  idle+diff gating, `local_sel` clamping after delete, variant add/switch. These
  are pure-ish functions of state and could be unit-tested by constructing a
  `History<Settings>` directly (as history.rs already does for `History<i32>`).
- **Out-of-range sidecar values:** the UI clamps, but `Document::from_ron` does
  not validate ranges, so a hand-edited sidecar can inject e.g. contrast > 1.
  Worth a `latent-edit`-side validation/clamp test (cross-crate, but the risk
  surfaces here because the GUI is the consumer that assumes clamped values).

---

## 7. Positives

- **Single output transform shared by 8-bit, 16-bit, and the live preview**
  (`to_display` → `to_srgb8`), making preview/file agreement structural and
  test-pinned to exact code values. This is the standout design choice.
- **Quantization is done right:** clamp to `[0,1]` happens in `to_display`
  (`:58`) *before* the `*255+0.5`/`*65535+0.5` round, so the `as u8`/`as u16`
  saturating cast never wraps and rounding is correct.
- **Per-format ICC embedding** with a real validated profile (`moxcms`), tested
  for both the 8-bit PNG and 16-bit TIFF encoders.
- **Lens FFI is genuinely careful:** null checks everywhere, `lf_free` on every
  path including early returns, RAII `Drop`, correct `CString` lifetimes, and a
  finiteness guard against divide-by-near-zero — with accurate SAFETY comments.
- **Undo/redo and autosave model is clean:** transactional gestures, one step
  per drag, no-op gestures leave no trace, autosave debounced to gesture
  completion and diffed against last-saved.
- **Robust to bad input:** missing sidecar, corrupt sidecar, bad/unsupported
  RAW, and missing lensfun DB all degrade gracefully rather than panicking.
- **Forward-compatible sidecar** (`Document::from_ron` rejects a newer version)
  and a **never-overwrite-the-sidecar** rule for auto-applied lens profiles
  (`gui.rs:42`).
- **CLI is idiomatic and correct:** typed errors, stderr + exit-1 on failure,
  GPU-with-CPU-fallback at the composition root.
- **Clippy clean, all tests green.**
