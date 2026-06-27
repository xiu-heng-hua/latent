# Embedded fonts

These typefaces are committed as binary assets and embedded into the binary with
`include_bytes!` (no font crate). Each ships its license text beside it.

| File | Family | Use | License |
| --- | --- | --- | --- |
| `Inter-Regular.ttf` | Inter | UI body / headings (proportional) | SIL OFL 1.1 — `Inter-OFL.txt` |
| `JetBrainsMono-Regular.ttf` | JetBrains Mono | numeric readouts (monospace, tabular digits) | SIL OFL 1.1 — `JetBrainsMono-OFL.txt` |
| `Phosphor-Regular.ttf` | Phosphor | UI icon glyphs (private-use codepoints) | MIT — `Phosphor-LICENSE.txt` |

The OFL requires its license to travel with the font and forbids selling the
font alone; committing the `*-OFL.txt` files satisfies this. The font files are
not renamed in a way that touches a Reserved Font Name. The app itself is MIT
(see the workspace `Cargo.toml`); MIT app + OFL/MIT fonts is compatible.

Phosphor codepoints are sourced from the upstream icon manifest; the
name→codepoint table lives in `latent-app/src/gui/icons.rs`.
