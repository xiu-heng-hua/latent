# Decision Registers — Code Review

Triage of the four **code-review** documents ([`../../code-review/`](../../code-review/)) — software-engineering quality (correctness, FFI/`unsafe` soundness, robustness, concurrency, idioms, tests), separate from the image-processing audits ([`../`](../)).

**How decided:** **Autonomously** (not via the interactive Q&A used for the image-processing audits), optimizing for correctness / safety / robustness — the same quality-maximizing posture the maintainer applied to the image-processing decisions. Each finding has an explicit Fix/Keep decision with rationale and a concrete action at `file:line`.

**These registers record intent only — no source code has been modified.**

| Register | Crates | Fix | Keep | Covered by IP |
|---|---|---|---|---|
| [`01-raw-and-image-foundations.md`](01-raw-and-image-foundations.md) | `latent-raw`, `latent-image` | ~13 | 2 | H1, M1 → IP-02 §2.8b |
| [`02-pipeline-and-edit-model.md`](02-pipeline-and-edit-model.md) | `latent-pipeline`, `latent-edit` | 14 | 0 | (H1 folds into IP-03 §2.8) |
| [`03-cpu-and-gpu-backends.md`](03-cpu-and-gpu-backends.md) | `latent-cpu`, `latent-gpu` | 8 | 2 | F1 → IP-05 §2.9 |
| [`04-export-lens-app.md`](04-export-lens-app.md) | `latent-export`, `latent-lens`, `latent-app` | 13 | 3 | M3 → IP-05 §2.3-L1 |

The code reviews confirmed the codebase is high quality (no gratuitous `unsafe`, sound bytemuck/std140 layout, clean rayon, RAII FFI, graceful no-GPU fallback). The fixes are robustness/safety hardening and test-gap closure, not redesigns.

---

## Overlaps deferred to the image-processing decisions (dedup)

Four code-review findings are remedied by an already-decided image-processing change; they are **registered** (so the code-review track is complete) but **not re-planned**:

| Code-review finding | Remedied by | Initiative |
|---|---|---|
| X-Trans accepted as Bayer (CR-01 M1); Foveon `filters==0` OOB panic (CR-01 H1) | IP-02 §2.8b sensor guard (`idata.filters`/`colors`) — **plus** a code-review note that the guard must run *before* any `cfa`/`cblack`/`gains` indexing, and a belt-and-suspenders `cfa ∈ 0..4` clamp | B (decode) |
| `point_curve` NaN-x panic (CR-02 H1) | IP-03 §2.8 monotone-cubic rewrite of the *same* function — make it total in one edit | A (tone) / F |
| GPU `resample.wgsl` missing `w≤0` guard (CR-03 F1) | IP-05 §2.9 (guard + `warp.wgsl`); §2.7 also adds Lanczos/bicubic + prefilter to both backends | E / G |
| Lens optical-center normalization (CR-04 M3) | IP-05 §2.3-L1 (scale center offset by `min(w,h)/2`) | C (lensfun) |

**Sanitize-on-load reconciliation:** CR-02 owns the **broad** `Settings::sanitize` (scrub non-finite floats + clamp ranges across the whole settings tree on `from_ron`); IP-03 §2.2's `SelectiveTone` clamp is a subset that defers to it. (Initiative F.)

---

## Net-new code-review work (by theme)

- **FFI / `unsafe` soundness (`latent-raw`/`latent-lens`):** honor `raw_pitch` row stride in the `from_raw_parts` load (CR-01 H2); `checked_mul` for `width*height` + fallible `ImageBuf::try_new` (CR-01 M2); `cfa ∈ 0..4` clamp at the FFI boundary (CR-01 H1); finiteness-guard the CA/vignetting coefficient mappers (CR-04 M4).
- **Data model & robustness (`latent-edit`):** broad `Settings::sanitize` on load (CR-02 M2); uniform `#[serde(default)]` + `Default` on mask-shape leaves so old sidecars survive shape evolution (CR-02 M1); bounded undo via `VecDeque` cap (CR-02 H2); `Backend: Send + Sync` (CR-02 M3).
- **Backends & GPU (`latent-cpu`/`latent-gpu`):** close the CPU/GPU equivalence-test gap + a Test/CPU/GPU conformance harness (CR-03 F3, CR-02 M4); `read_staging` returns a recoverable error on device loss → CPU fallback (CR-03 F5); keep the image GPU-resident + pool buffers to cut per-call round-trips (CR-03 F4); port `combine`/`apply_radial_gain` to WGSL (CR-03 F6).
- **Export, CLI & app (`latent-export`/`latent-app`):** reject unknown export extensions with a typed error instead of silently writing untagged files (CR-04 H2); `save`/`save_16` honor their `ImageResult` contract + zero-dimension guard (CR-04 M1/M2); make the 16-bit path reachable from the CLI (`--depth`, auto for tiff/png) (CR-04 L2); move render/export off the egui UI thread to a worker (CR-04 H1); CLI exit codes, doc nits, and test coverage (lens-lookup stub, CLI arg parsing, FFI error paths).

All of this feeds the Kanban plan in [`../../kanban/`](../../kanban/).
