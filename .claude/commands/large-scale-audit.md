---
description: Run a comprehensive, source-grounded, multi-agent audit of the current project — an engineering code review plus a domain-correctness audit verified against primary literature, plus a curated theory-resources list — with all deliverables written under audits/ and downloaded references under docs/.
argument-hint: [optional focus, e.g. "color pipeline", "code-review only", or a subsystem name]
---

You are running a **large-scale audit** of the current project. Produce three deliverables — a software **code review**, a **domain-correctness audit** verified against authoritative primary sources, and a curated **theory-resources** list — by orchestrating parallel subagents and synthesizing their work. Optional focus from the user: `$ARGUMENTS` (if empty, audit the whole project).

## Operating principles (apply throughout)
1. **Verify, don't recall.** Every domain/correctness claim must be checked against an **authoritative primary source** (the canonical paper, the standard, the official docs, or the reference implementation's source) — *not* training data. Download each cited reference into `docs/` (prefix by domain, e.g. `docs/color-*.pdf`); cite specific sections/equations/page numbers. Where a paywalled standard can't be downloaded, cite it precisely without redistributing.
2. **Ground the code review in the code.** Every finding gets a `file:line`. When you suspect a bug, **confirm it with a throwaway test/snippet before reporting it**, and record disproven suspicions as non-issues. Avoid false positives; distinguish real defects from style.
3. **Conserve orchestrator context.** Subagents read the files and **write their own deliverable**, returning only a concise summary (counts by severity, top findings, files written/downloaded). You synthesize from those summaries plus targeted reads.
4. **Use parallel subagents** for independent domains — launch them concurrently (multiple agent calls in one message).
5. Use the consistent **severity** scale `Critical / High / Medium / Low / Nit` and, for the domain audit, **verdicts** `Correct / Correct-with-caveats / Questionable / Incorrect`.

## Phase 1 — Orient
- Map the repo: structure, languages, build system, modules/crates, external dependencies and FFI seams, tests. Read the top-level manifest(s) and the module/header doc-comments to learn the project's **domain** and its **core technical subsystems** (these become the audit domains — the analog of a render pipeline's stages).
- Note any `$ARGUMENTS` focus and scope the work to it.

## Phase 2 — Baseline (ground findings in reality)
- Run the project's build, tests, linter, and formatter (e.g. for Rust: `cargo fmt --check`, `cargo build --workspace --all-targets`, `cargo test --workspace`, `cargo clippy --workspace --all-targets`). Record pass/fail, test counts, and any warnings. Findings should go *beyond* what the toolchain already catches.
- Check available tooling for deliverables: a container runtime (`podman`/`docker`) for LaTeX→PDF, and internet access for source verification.
- Create the deliverable layout: `audits/code-review/`, `audits/<domain>/` (e.g. `audits/image-processing/`), and `docs/`.

## Phase 3 — Decompose
- **Code review** — split by crate/module/domain into a handful of cohesive review areas (FFI/unsafe, core algorithms, orchestration/data-model, backends/concurrency, I/O & UI, …).
- **Domain-correctness audit** — split by the project's actual technical domains (each a coherent subsystem whose correctness can be checked against external references).
- **Theory resources** — one research task: a curated, *verified* reading list for someone with strong fundamentals but no prior domain knowledge.

## Phase 4 — Fan out (parallel subagents)
Launch the agents concurrently. Give each: the project context, its precise scope (files), the operating principles above, the exact output path, and the instruction to **write its report and return a concise summary**.

- **Code-review agents** (one per review area): rigorous engineering review — correctness bugs, panics/overflow, FFI/`unsafe` soundness, concurrency/data races, error handling, API design, idioms, performance, **test-coverage gaps**, and positives worth keeping. Output `audits/code-review/NN-<area>.md`: overview, findings by severity with `file:line` + concrete fix, a soundness/safety subsection where relevant, test gaps, positives.
- **Domain-audit agents** (one per domain): verify the algorithms/math/conventions against the cited primary sources; for each claim give a **verdict + citation**; flag the highest-risk *silent-wrongness* issues. Output `audits/<domain>/NN-<topic>.md` with point-by-point verification, a findings-by-severity section, and a references section with URLs. Use LaTeX-style math (`$…$`, `$$…$$`, `\begin{bmatrix}`) where formulas matter, so it can be compiled later. Download cited PDFs to `docs/` with the domain prefix.
- **Resources agent**: a curated, annotated list (free + paid), each entry verified to exist with a working link, organized by topic, with a sequenced reading path, an "if you only read five things" shortlist, and a table mapping each algorithm the project actually implements to its canonical source. Output `audits/theory-resources.md`.

## Phase 5 — Synthesize
- Write `audits/README.md`: the headline assessment, the baseline status, the methodology, and a **consolidated, de-duplicated, severity-ranked findings register** spanning both tracks. **Call out cross-corroborated findings** — anything two independent agents found from different angles is the strongest signal. Link every per-domain report and the resources list. Add a suggested remediation order.

## Phase 6 — Compile (choose the format per content)
- Keep textual deliverables (code review, resources) as **Markdown** (clickable `file:line`, easy diffs).
- Compile the **formula-heavy** domain audit to a typeset **PDF** when a container runtime is available: build a small image from `pandoc/latex` (add a Unicode font, e.g. `apk add font-dejavu`, and `tlmgr install` any missing packages such as `fvextra`/`xurl`), then `pandoc` the combined Markdown with `--pdf-engine=xelatex`, a Unicode `mainfont`, `--toc --number-sections`. On rootless Podman, run the container as `--user 0` and disable the SELinux label on the bind mount so output is owned by the host user; map exotic glyphs (e.g. ✓) via `newunicodechar` if the font lacks them. Render a page or two to PNG and visually confirm before finishing.

## Constraints
- Deliverables under `audits/`; downloaded references under `docs/`. Do **not** modify project source during the audit.
- Scale effort to the request: a quick check = a few agents, single-vote findings; "comprehensive/thorough" = a larger fan-out and a synthesis pass. Lean thorough for audits.
- Be honest about what couldn't be verified.

## Follow-ups (offer, don't auto-run)
After the audit, offer to continue the workflow: **triage** each finding into per-domain decision registers (`audits/decisions/`) — keep/fix with rationale, de-duplicated and consistency-checked — then a **Kanban implementation plan** (`audits/kanban/`) of independent epics and self-contained cards with implementer heads-ups, then **implement** epic-by-epic via subagents (one commit per epic, baseline green after each).
