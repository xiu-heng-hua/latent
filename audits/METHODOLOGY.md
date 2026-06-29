# What we did — audit, decisions, plan, and implementation

A record of the end-to-end effort on `latent` (a RAW photo developer): a large-scale audit, a triage of every finding into decisions, a Kanban implementation plan, and the implementation itself. The reusable workflow is captured as the [`/large-scale-audit`](../.claude/commands/large-scale-audit.md) command.

**Everything under `audits/` and `docs/` is working/planning material — it is intentionally kept out of the git history.** The code changes live on the `pipeline-overhaul` branch as nine commits with neutral messages and comments (no references to this material).

---

## Phase 1 — Large-scale audit

**Goal:** a comprehensive software code review, a domain-correctness audit of the image-processing math verified against primary literature, and a curated theory-resources list.

**Method:** ten parallel subagents, each owning one domain, writing its own report and returning a concise summary (to keep the orchestrator's context lean). Two guard-rails: domain claims were verified against **authoritative primary sources** (papers/standards/reference source — downloaded to `docs/`, cited inline), and code-review findings were grounded in the code and **reproduced by a throwaway test** before being reported. A clean baseline (`fmt`/`build`/`clippy`/50 tests) was established first so findings went beyond what the toolchain catches.

**Deliverables**
- Code review — [`code-review/`](code-review/): `latent-raw`+`latent-image`, `latent-pipeline`+`latent-edit`, `latent-cpu`+`latent-gpu`, `latent-export`+`latent-lens`+`latent-app`.
- Image-processing audit — [`image-processing/`](image-processing/): color science, RAW decode & demosaic, tone & color grading, spatial & frequency filters, geometry & optics — plus a compiled PDF, [`latent-image-processing-audit.pdf`](latent-image-processing-audit.pdf) (73 pages), typeset from the formula-heavy Markdown via `pandoc` + XeLaTeX in a Podman container.
- Theory resources — [`theory-resources.md`](theory-resources.md): ~40 verified resources across 12 categories.
- Executive summary — [`README.md`](README.md): consolidated, severity-ranked findings register with cross-corroborated findings highlighted.
- Reference library — [`../docs/`](../docs/): 17 downloaded primary sources (papers, standards, lensfun source) cited throughout.

**Outcome:** the codebase was assessed as high quality; findings were edge-case correctness, a Critical lens-normalization mismatch, and robustness gaps. Three findings were discovered independently by two agents (GPU `w≤0` divergence, X-Trans mis-handling, lens optical-center) — the strongest signal of validity.

## Phase 2 — Decisions (triage every finding)

Each finding was triaged into **keep-as-is** or **fix**, with rationale, the concrete action (`file:line`), and cross-dependencies — recorded in decision registers.

- Image-processing findings — triaged **interactively** (point-by-point, in document order): [`decisions/01..05`](decisions/) + [`decisions/README.md`](decisions/README.md). 50 decisions; the posture was consistently quality-maximizing.
- A **consistency pass** (see `decisions/README.md` → *Consistency reconciliation*) confirmed no contradictions, applied four quality upgrades (always-monotone S-curve, monotone-cubic curves, perceptual-domain luma sharpen, higher-order resampling), and reconciled overlapping decisions onto a single perceptual-lightness definition (CIE L\*).
- Code-review findings — triaged **autonomously** with the same quality posture: [`decisions/code-review/`](decisions/code-review/). Overlaps with image-processing decisions were cross-referenced, not double-planned.

The decisions cohere into a few cross-cutting initiatives: a **perceptual color core** (D50 working space + Bradford adaptation + Lab/LCh + L\*), **sensor-metadata correctness**, **lensfun fidelity**, a **full dark-channel dehaze**, **higher-order resampling + GPU parity**, and **robustness / sanitize-on-load**.

## Phase 3 — Kanban implementation plan

The ~85 fix-decisions were decomposed into **9 independent epics** and **54 self-contained cards**, each with an implementer **heads-up** (approach, gotchas, cross-backend obligations) and acceptance tests — [`kanban/`](kanban/) (board index + one file per epic). A dependency graph and build order were derived from the cross-cutting initiatives.

## Phase 4 — Implementation

Each epic was implemented by a focused subagent (the largest, GPU-heavy epic split into two passes), verified (`fmt`/`clippy`/full test suite) by the orchestrator, then committed — **one conventional commit per epic**, in dependency order. Code comments and commit messages are neutral and do not reference this material.

Branch `pipeline-overhaul`, nine commits:

| Commit | Epic |
|---|---|
| `fix(edit): harden settings loading, history, and backend bounds` | E7 — data model & robustness |
| `feat(color): adopt a standard D50 color-managed pipeline with Lab/LCh` | E0 — color core |
| `fix(raw): correct sensor levels and harden decoding` | E2 — decode & sensor |
| `fix(lens): apply lens-correction coefficients at the correct scale` | E3 — lensfun fidelity |
| `feat(dehaze): estimate airlight and refine transmission` | E4 — dehaze |
| `feat(grading): move tone and color tools into perceptual spaces` | E1 — tone & grading |
| `feat(filters): sharpen and denoise in perceptual spaces` | E5 — spatial filters |
| `feat(render): higher-order resampling and full GPU parity` | E6 — resampling & GPU |
| `feat(app): color-managed export, 16-bit output, and a responsive editor` | E8 — export, CLI & app |

**Final state:** `fmt` clean, `clippy` zero warnings, **313 tests pass** (from ~50). Each commit is an independently green checkpoint. Notable engineering judgments (all documented in code, all keeping CPU↔GPU results equal): the capture illuminant is estimated from the white-balanced neutral (no explicit metadata for it); the lens code was adapted to the locally-linked lensfun 0.3.x ABI; and the two L\*-domain GPU ops (chroma-preserving saturation, luma unsharp) delegate to the CPU so the backends match exactly rather than risk transcribing the Lab chain into a shader.

---

## Tooling notes
- **PDF:** built a small image `FROM pandoc/latex` with DejaVu fonts + `fvextra`/`xurl`/`newunicodechar`; ran rootless Podman as `--user 0` with the SELinux label disabled on the bind mount; compiled with `--pdf-engine=xelatex` and a Unicode `mainfont`. The combined source is reproducible from the per-domain Markdown.
- **FFI:** the project links LibRaw (decode) and lensfun (lens DB) via `pkg-config`.

## Reproducing this on another project
Run [`/large-scale-audit`](../.claude/commands/large-scale-audit.md) (optionally with a focus argument). It performs Phase 1, and offers to continue into the decisions → Kanban → implementation phases.
