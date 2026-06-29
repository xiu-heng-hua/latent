# Theory Resources for Building a RAW Photo Developer

A curated, annotated, and link-verified reading list for understanding **all the theory**
behind a tool like Lightroom / darktable / RawTherapee — decoding a camera RAW file and
developing it into a finished image. It is written to ground the `latent` project (a Rust
RAW developer), so every resource is tagged with the `latent` subsystem it maps to.

## Who this is for

A reader with a strong background in **engineering, mathematics, and theoretical CS**, but
**no prior exposure** to imaging, color science, photography, or RAW-software internals.
Accordingly this list **skips** generic intros to programming, linear algebra, and Fourier
analysis (assumed known) and **concentrates on the imaging / color / photographic-engineering
specifics** you actually lack. Rigorous and advanced sources are included where they are the
right ones.

## How to use this list

- Every entry is tagged **TYPE** (book / paper / course / official-docs / source-code /
  website / standard), **ACCESS** (Free / Paid, with the legitimate route), and a
  **difficulty** (Foundational / Intermediate / Advanced).
- Links were **verified live** during compilation (see the "Could not verify" note at the end
  for the few caveats). Prefer the **primary/canonical** source; free author PDFs are given
  where the venue is paywalled.
- The **`latent` subsystem** tag tells you which crate/module each resource explains:
  - `latent-raw` — RAW decode (LibRaw FFI), black/white levels, CFA, camera matrix metadata
  - `latent-image` — linear-light buffers, `color.rs` (XYZ / sRGB / ProPhoto-ROMM / camera matrix), `tone.rs` (perceptual-domain tone curves)
  - `latent-pipeline` — the fixed ordered render pipeline, SOURCE→OUTPUT geometry
  - `latent-cpu` / `latent-gpu` — pixel primitives: demosaic, box/Gaussian blur, unsharp/clarity (midtone-weighted local contrast), bilateral denoise, dark-channel dehaze, bilinear resample, warp, radial gain
  - `latent-lens` — lensfun lookup → lens correction model (distortion, lateral CA, vignetting)
  - `latent-edit` / `latent-export` — edit-state model, output transform & encoding

Several of these references are already mirrored in this repo's `docs/` directory (DNG spec,
Malvar–He–Cutler, Tomasi bilateral, He dehaze CVPR/TPAMI + IPOL, Poynton FAQs, sRGB & ROMM
standards, Szeliski Ch. 3, lensfun & PanoTools model pages). That local set is the project's
"primary sources" cache; this document is the broader, annotated map around it.

---

## Suggested reading path (zero → able to build each subsystem)

A sequenced roadmap. Entries are referenced by their number within each category, e.g. *(2.5)*
= category 2, item 5.

1. **Orient yourself to the whole pipeline first.** Read Michael Brown's in-camera pipeline
   tutorial *(2.5)* and skim Szeliski Ch. 2–3 *(6.7)*. Now you know the stages and their order
   — the same order `latent-pipeline::render` enforces.
2. **Get the color-science vocabulary.** Work through Poynton's Color & Gamma FAQs *(1.5b)*
   (short, free, authoritative), then keep Bruce Lindbloom's site *(1.6)* open as your math
   reference for RGB↔XYZ matrices and chromatic adaptation. This is exactly what
   `latent-image/src/color.rs` implements. Use Reinhard *Color Imaging* *(1.4)* as the readable
   textbook backbone; reach for Wyszecki & Stiles *(1.3)* only when you need the canonical data.
3. **Understand where the pixels come from.** Read the demosaicing tutorial *(3.10)* and the
   Malvar–He–Cutler paper *(3.6)* with the IPOL write-up + reference code *(3.8)* beside it —
   that pair *is* the demosaic in `latent-cpu`. Read the DNG spec's color sections *(2.3)* to see
   how the camera matrix, black/white levels, and CFA are described in metadata that
   `latent-raw` parses.
4. **Tone & gamma.** Read Poynton's Gamma FAQ again, then `tone.rs`'s premise becomes obvious:
   curves act in a perceptual (non-linear) domain. For highlight/HDR handling read the Reinhard
   2002 operator *(5.2)*, Hable's filmic notes *(5.4)*, and the ACES output-transform docs *(5.3)*
   to understand scene-referred → display-referred rendering.
5. **Spatial & edge-preserving filtering.** Szeliski Ch. 3 *(6.7)* for box/Gaussian/unsharp;
   then Tomasi–Manduchi *(7.1)* for the bilateral filter behind `latent-cpu::denoise`, and the
   bilateral course *(7.4)* for intuition. Durand–Dorsey *(7.2)* shows the base/detail split that
   underlies local-contrast/clarity.
6. **Dehazing.** He–Sun–Tang dark channel prior *(8.6)* + the IPOL implementation *(8.7)*; this
   is `latent-cpu::dehaze` end to end.
7. **Geometry & lenses.** Hartley & Zisserman Ch. on homographies *(9.1)* + Zhang's calibration
   paper *(9.2)* for the projective math behind keystone/perspective. Then the lensfun model docs
   *(9.5)* and PanoTools model *(9.4)* for the exact distortion/vignetting/TCA polynomials
   `latent-lens` maps and `latent-pipeline` applies; OpenCV's distortion docs *(9.3)* give the
   Brown–Conrady reference form. Ray *(10.6)* for the optics (cos⁴ vignetting, lateral CA).
8. **Resampling done right.** Heckbert's thesis *(6.8)* and Thévenaz–Blu–Unser *(6.10a)* explain
   why bilinear is fine for unit-scale warps but aliases on minification — the exact tradeoff
   documented in `latent-cpu::sample_bilinear`.
9. **GPU acceleration.** The WebGPU + WGSL specs *(12.11)* and wgpu docs *(12.12)* for
   `latent-gpu`'s WGSL kernels; GPU Gems *(12.13)* for image-processing patterns.
10. **Study real implementations.** Read dcraw.c *(2.4 / 11.10)* as a primary artifact, then
    darktable *(11.8)* and RawTherapee *(11.7)* sources, with Aurélien Pierre's scene-referred
    articles *(11.9)* for the modern workflow philosophy `latent` follows.

---

## 1. Foundational color science & colorimetry

- **Color FAQ & Gamma FAQ** — Charles Poynton; TYPE: website + free PDF; ACCESS: **Free**
  ([Color FAQ PDF](https://www.poynton.ca/pdf/ColourFAQ.pdf),
  [Gamma FAQ PDF](https://www.poynton.ca/faq/gammafaq/GammaFAQ.pdf)).
  Concise, authoritative answers on color encoding, primaries, white points, and transfer
  functions/gamma. The fastest way to acquire correct vocabulary. Maps to
  `latent-image/src/{color.rs,tone.rs}`. *Foundational.*
- **Bruce Lindbloom's website** — Bruce Lindbloom; TYPE: website; ACCESS: **Free**
  ([brucelindbloom.com](http://www.brucelindbloom.com/) — use **http://**, the HTTPS cert is
  broken). Working RGB↔XYZ conversion matrices, chromatic-adaptation (Bradford) math, and color
  calculators. This is effectively the spec for `color.rs`'s matrix construction
  (`rgb_to_xyz`, chromatic adaptation, primaries). *Foundational.*
- **Color Imaging: Fundamentals and Applications** — Reinhard, Khan, Akyüz & Johnson, 2008,
  A K Peters / CRC Press; TYPE: book; ACCESS: **Paid**
  ([Taylor & Francis, DOI 10.1201/b10637](https://www.taylorfrancis.com/books/color-imaging-erik-reinhard-erum-arif-khan-ahmet-oguz-akyuz-garrett-johnson/10.1201/b10637)).
  The single most readable end-to-end textbook covering light physics, human vision,
  colorimetry, and device color. Best textbook backbone for the whole `latent-image` color
  layer. *Intermediate.*
- **Digital Video and HD: Algorithms and Interfaces** — Charles Poynton, 2nd ed., 2012, Morgan
  Kaufmann/Elsevier; TYPE: book; ACCESS: **Paid**
  ([author's book page](https://poynton.ca/DVAI/)). The definitive rigorous treatment of gamma,
  transfer functions, and color encoding. Directly informs `tone.rs`'s encode→curve→decode
  round-trip and the export transfer function. *Intermediate.*
- **Color Appearance Models** — Mark D. Fairchild, 3rd ed., 2013, Wiley; TYPE: book; ACCESS:
  **Paid** ([Wiley](https://www.wiley.com/en-us/Color+Appearance+Models,+3rd+Edition-p-9781119967033)).
  Chromatic adaptation transforms (Bradford, CAT02) and appearance phenomena. Justifies *why*
  `latent` pins the working space to D65 to avoid a separate adaptation step. *Advanced.*
- **Color Science: Concepts and Methods, Quantitative Data and Formulae** — Wyszecki & Stiles,
  2nd ed., 1982, Wiley; TYPE: book; ACCESS: **Paid**
  ([Wiley](https://www.wiley.com/en-us/Color+Science:+Concepts+and+Methods,+Quantitative+Data+and+Formulae,+2nd+Edition-p-9780471399186)).
  The canonical reference of colorimetric data, observers, and formulae — the place to confirm a
  number rather than to learn from. *Advanced (reference).*
- **The Reproduction of Colour** — R. W. G. Hunt, 6th ed., 2004, Wiley; TYPE: book; ACCESS:
  **Paid** ([Wiley](https://www.wiley.com/en-us/The+Reproduction+of+Colour,+6th+Edition-p-9780470024256)).
  Classic treatment of color reproduction across media; deep background for output rendering.
  *Advanced.*
- **CIE 015:2018 Colorimetry, 4th ed.** — International Commission on Illumination; TYPE:
  standard; ACCESS: **Paid**
  ([CIE webshop](https://cie.co.at/publications/colorimetry-4th-edition)). The primary standard
  for standard observers, illuminants, and tristimulus computation. The source of truth behind
  the constants in `color.rs`. *Advanced (standard).*

## 2. Digital camera image formation, sensors & the RAW pipeline

- **Understanding the In-Camera Image Processing Pipeline for Computer Vision** — Michael S.
  Brown, CVPR 2016 tutorial; TYPE: tutorial/slides; ACCESS: **Free**
  ([slides page](https://www.eecs.yorku.ca/~mbrown/CVPR2016_Brown.html)). A clear end-to-end
  walkthrough of RAW→sRGB (black level, white balance, demosaic, color/tone, output). The single
  best free orientation to the whole pipeline `latent-pipeline` re-implements. *Foundational.*
- **Adobe DNG (Digital Negative) Specification** — Adobe, v1.6.0.0, 2021; TYPE: official-docs;
  ACCESS: **Free**
  ([landing](https://helpx.adobe.com/camera-raw/digital-negative.html),
  [PDF](https://helpx.adobe.com/content/dam/help/en/photoshop/pdf/dng_spec_1_6_0_0.pdf);
  local copy `docs/color-dng-spec-1.4.0.0.pdf`, `docs/demosaic-dng-spec-1.6.0.0.pdf`).
  Defines the camera-negative container and, crucially, how `ColorMatrix`, black/white levels,
  and the CFA pattern are encoded — exactly the metadata `latent-raw` consumes. *Intermediate.*
- **dcraw + LibRaw** — Dave Coffin / LibRaw project; TYPE: source-code + docs; ACCESS:
  **Free/open-source** ([dcraw](https://www.dechifro.org/dcraw/),
  [LibRaw docs](https://www.libraw.org/docs)). The reference RAW decoders for hundreds of
  cameras. LibRaw is the actual library behind `latent-raw`'s FFI boundary; dcraw.c is the
  primary artifact to read for how unpacking and linearization really work. *Intermediate.*
- **A Software Platform for Manipulating the Camera Imaging Pipeline** — Karaimer & Brown, ECCV
  2016; TYPE: paper; ACCESS: **Free** author page / **Paid** Springer
  ([project + PDF](https://karaimer.github.io/camera-pipeline/),
  [DOI 10.1007/978-3-319-46448-0_26](https://doi.org/10.1007/978-3-319-46448-0_26)). A research
  platform that exposes each ISP stage independently — a model of how to factor the pipeline,
  echoing `latent`'s fixed-order, primitive-per-stage design. *Intermediate.*
- **Image Sensors and Signal Processing for Digital Still Cameras** — Junichi Nakamura (ed.),
  2005, CRC Press; TYPE: book; ACCESS: **Paid**
  ([Taylor & Francis](https://www.taylorfrancis.com/books/9780849335457)). CCD/CMOS sensor
  physics, noise, and the acquisition chain. The "below the RAW file" theory: why black levels,
  saturation, and linearization exist. *Advanced.*

## 3. Demosaicing

- **High-Quality Linear Interpolation for Demosaicing of Bayer-Patterned Color Images** —
  Malvar, He & Cutler, ICASSP 2004; TYPE: paper; ACCESS: **Free** at Microsoft Research /
  **Paid** IEEE
  ([MSR PDF](https://www.microsoft.com/en-us/research/publication/high-quality-linear-interpolation-for-demosaicing-of-bayer-patterned-color-images/),
  [DOI 10.1109/ICASSP.2004.1326587](https://doi.org/10.1109/ICASSP.2004.1326587); local copy
  `docs/demosaic-malvar-he-cutler-2004.pdf`). **The** primary source for `latent`'s demosaic: a
  5×5 linear filter with cross-channel Laplacian correction. *Intermediate.*
- **Malvar-He-Cutler Linear Image Demosaicking** — Pascal Getreuer, IPOL 2011; TYPE: paper +
  reference code; ACCESS: **Free** (open access)
  ([article](https://www.ipol.im/pub/art/2011/g_mhcd/),
  [DOI 10.5201/ipol.2011.g_mhcd](https://doi.org/10.5201/ipol.2011.g_mhcd)). Peer-reviewed
  write-up with BSD reference C code and an online demo. Read this *with* (3.6) — it is the
  implementation-grade companion that disambiguates the filter taps. *Intermediate.*
- **Demosaicking: Color Filter Array Interpolation** — Gunturk, Glotzbach, Altunbasak, Schafer &
  Mersereau, IEEE Signal Processing Magazine 2005; TYPE: paper; ACCESS: **Free** author PDF /
  **Paid** IEEE
  ([PDF](https://www.ece.lsu.edu/ipl/papers/IEEE_SPM2005.pdf),
  [DOI 10.1109/MSP.2005.1407714](https://doi.org/10.1109/MSP.2005.1407714)). The classic
  accessible tutorial on single-chip CFA interpolation — the best first read on demosaicing.
  *Foundational.*
- **Image Demosaicing: A Systematic Survey** — Li, Gunturk & Zhang, Proc. SPIE VCIP 2008; TYPE:
  paper (survey); ACCESS: **Free** author PDF / **Paid** SPIE
  ([PDF](https://www4.comp.polyu.edu.hk/~cslzhang/paper/conf/demosaicing_survey.pdf),
  [DOI 10.1117/12.766768](https://doi.org/10.1117/12.766768)). Taxonomy and comparison across the
  algorithm space — context for why Malvar–He–Cutler is a strong default. *Intermediate.*
- **Comparison of Color Demosaicing Methods** — Losson, Macaire & Yang, *Advances in Imaging and
  Electron Physics* vol. 162, 2010; TYPE: paper/chapter; ACCESS: **Free** preprint (HAL) /
  **Paid** Elsevier
  ([HAL](https://hal.science/hal-00683233/),
  [DOI 10.1016/S1076-5670(10)62005-8](https://doi.org/10.1016/S1076-5670(10)62005-8)). An
  extensive comparative evaluation; deeper reference if you extend beyond bilinear/MHC. *Advanced.*

## 4. Color management & ICC profiles

- **ICC.1:2022 specification (ICC profile format)** — International Color Consortium; TYPE:
  official standard; ACCESS: **Free** from color.org (ISO 15076-1 edition is paid)
  ([free PDF](https://www.color.org/specification/ICC.1-2022-05.pdf),
  [spec index](https://www.color.org/icc_specs2.xalter)). Defines the ICC profile format and
  color-transform architecture. The standard `latent-export` would target when emitting embedded
  output profiles. *Advanced (standard).*
- **Little CMS (lcms2) documentation** — Marti Maria; TYPE: official-docs + open-source library;
  ACCESS: **Free** ([littlecms.com](https://littlecms.com),
  [tutorial/API PDF](https://gensoft.pasteur.fr/docs/lcms2/2.9/LittleCMS2.9%20tutorial.pdf),
  [source](https://github.com/mm2/Little-CMS)). The de-facto open-source CMM; its tutorial shows
  how ICC transforms are actually built and applied — the practical counterpart to the spec.
  *Intermediate.*
- **Digital Color Imaging Handbook** — Gaurav Sharma (ed.), CRC Press (1st 2003 / 2nd 2014);
  TYPE: book; ACCESS: **Paid**
  ([Routledge](https://www.routledge.com/Digital-Color-Imaging-Handbook/Sharma-Sharma-Bala/p/book/9780849309007)).
  Authoritative chapters on color management, device characterization, and ICC workflows. Deeper
  theory behind profile-based color than the spec alone gives. *Advanced.*

## 5. Tone reproduction, gamma, HDR & tone mapping

- **Photographic Tone Reproduction for Digital Images** — Reinhard, Stark, Shirley & Ferwerda,
  ACM TOG / SIGGRAPH 2002; TYPE: paper; ACCESS: **Free** author tech-report PDF / **Paid** ACM
  ([Utah PDF](https://www.cs.utah.edu/docs/techreports/2002/pdf/UUCS-02-001.pdf),
  [DOI 10.1145/566570.566575](https://doi.org/10.1145/566570.566575)). The "Reinhard operator" —
  Zone-System-inspired tone mapping. Foundational theory for highlight compression and
  shadow/highlight tone shaping in `tone.rs`. *Intermediate.*
- **High Dynamic Range Imaging: Acquisition, Display, and Image-Based Lighting** — Reinhard,
  Heidrich, Debevec, Pattanaik, Ward & Myszkowski, 2nd ed., 2010, Morgan Kaufmann; TYPE: book;
  ACCESS: **Paid**
  ([Elsevier](https://shop.elsevier.com/books/high-dynamic-range-imaging/reinhard/978-0-12-374914-7)).
  The textbook on HDR capture, encoding, and tone reproduction — the rigorous backing for
  highlight handling and scene-referred rendering. *Advanced.*
- **ACES Output Transforms documentation** — Academy of Motion Picture Arts and Sciences; TYPE:
  official-docs; ACCESS: **Free** (CC BY 4.0)
  ([docs.acescentral.com](https://docs.acescentral.com/),
  [output transforms](https://docs.acescentral.com/system-components/output-transforms/)). The
  reference scene-referred → display-referred pipeline (rendering/output transforms). The model
  for a principled `latent-export` output transform. *Advanced.*
- **Filmic Tonemapping Operators** — John Hable, 2010; TYPE: website/blog; ACCESS: **Free**
  ([filmicworlds.com](https://filmicworlds.com/blog/filmic-tonemapping-operators/)). Practical,
  shader-level comparison of filmic tone-mapping curves (incl. the Uncharted 2/Hable operator).
  Directly applicable to a display-rendering curve before export. *Intermediate.*
- **Filmic Blender & AgX** — Troy Sobotka; TYPE: website / GitHub; ACCESS: **Free**
  ([filmic-blender](https://github.com/sobotka/filmic-blender),
  [AgX](https://github.com/sobotka/AgX)). Scene-referred image-formation view transforms (AgX is
  now Blender's default). The clearest open argument for a scene-referred working pipeline like
  `latent`'s. *Intermediate.*

## 6. General digital image processing (filtering, sampling, interpolation/resampling)

- **Computer Vision: Algorithms and Applications** — Richard Szeliski, 2nd ed., 2022, Springer;
  TYPE: book; ACCESS: **Free** PDF (personal use) + **Paid** print
  ([book site](https://szeliski.org/Book/),
  [Springer](https://link.springer.com/book/10.1007/978-3-030-34372-9); local copy of the image-
  processing chapter at `docs/geometry-szeliski-03-image-processing.pdf`). Ch. 3 (image
  processing — linear filtering, pyramids, geometric transforms) and the geometry chapters cover a
  large fraction of `latent-cpu`/`latent-pipeline` in one place. *Intermediate.*
- **Fundamentals of Texture Mapping and Image Warping** — Paul S. Heckbert, M.Sc. thesis, UC
  Berkeley 1989; TYPE: thesis; ACCESS: **Free**
  ([Berkeley page](https://www2.eecs.berkeley.edu/Pubs/TechRpts/1989/5504.html),
  [PDF](http://www2.eecs.berkeley.edu/Pubs/TechRpts/1989/Archive/CSD-89-516.pdf)). The canonical
  theory of resampling filters and antialiasing for arbitrary warps. Explains exactly the
  prefilter/aliasing tradeoff documented in `latent-cpu::sample_bilinear`. *Advanced.*
- **Image Interpolation and Resampling** — Thévenaz, Blu & Unser, in *Handbook of Medical
  Imaging*, 2000; TYPE: book chapter; ACCESS: **Free** author PDF
  ([EPFL PDF](https://bigwww.epfl.ch/publications/thevenaz9901.pdf)). A rigorous survey of
  interpolation kernels (nearest, linear, cubic/Keys, B-splines, sinc). The reference for
  choosing/upgrading the sampler in `resample`/`warp`. *Advanced.*
- **Digital Image Processing** — Gonzalez & Woods, 4th ed., 2018, Pearson; TYPE: book; ACCESS:
  **Paid** ([Pearson](https://www.pearson.com/en-us/subject-catalog/p/digital-image-processing/P200000003224/9780137848560),
  [companion site](https://imageprocessingplace.com)). The standard DIP textbook: sampling,
  transforms, spatial/frequency filtering, restoration. Solid reference for the blur/sharpen
  primitives. *Foundational/Intermediate.*
- **Digital Image Warping** — George Wolberg, 1990, IEEE CS Press; TYPE: book; ACCESS: **Paid**
  ([Wiley](https://www.wiley.com/en-us/Digital+Image+Warping-p-9780818689444)). Book-length
  treatment of geometric transforms, separable/two-pass warps, and resampling — depth behind the
  geometry stage. *Advanced.*
- **Cubic Convolution Interpolation for Digital Image Processing** — Robert G. Keys, IEEE TASSP
  1981; TYPE: paper; ACCESS: **Free** mirror / **Paid** IEEE
  ([PDF](http://www.ncorr.com/download/publications/keysbicubic.pdf),
  [DOI 10.1109/TASSP.1981.1163711](https://doi.org/10.1109/TASSP.1981.1163711)). Derives the
  bicubic kernel — the natural quality upgrade over `latent`'s bilinear sampler. *Intermediate.*

## 7. Edge-preserving filtering & computational photography

- **Bilateral Filtering for Gray and Color Images** — Tomasi & Manduchi, ICCV 1998; TYPE: paper;
  ACCESS: **Free** author PDF / **Paid** IEEE
  ([PDF](https://users.soe.ucsc.edu/~manduchi/Papers/ICCV98.pdf),
  [DOI 10.1109/ICCV.1998.710815](https://doi.org/10.1109/ICCV.1998.710815); local copy
  `docs/spatial-bilateral-tomasi-1998.pdf`). The primary source for the bilateral filter behind
  `latent-cpu::denoise` (domain × range Gaussian weighting, luma/chroma split). *Intermediate.*
- **A Gentle Introduction to Bilateral Filtering and its Applications** (SIGGRAPH course) +
  **Bilateral Filtering: Theory and Applications** — Paris, Kornprobst, Tumblin & Durand; TYPE:
  course + monograph; ACCESS: **Free**
  ([course](https://people.csail.mit.edu/sparis/bf_course/),
  [monograph PDF](https://people.csail.mit.edu/sparis/publi/2009/fntcgv/Paris_09_Bilateral_filtering.pdf)).
  The best intuition-building treatment plus fast-implementation theory for the denoise primitive.
  *Intermediate.*
- **Fast Bilateral Filtering for the Display of HDR Images** — Durand & Dorsey, SIGGRAPH 2002;
  TYPE: paper; ACCESS: **Free** author page / **Paid** ACM
  ([MIT page](https://people.csail.mit.edu/fredo/PUBLI/Siggraph2002/),
  [DOI 10.1145/566654.566574](https://doi.org/10.1145/566654.566574)). The base/detail
  decomposition via bilateral filtering — the conceptual basis for local contrast / clarity
  (`latent`'s midtone-weighted local contrast). *Intermediate.*
- **Guided Image Filtering** — He, Sun & Tang, ECCV 2010 / IEEE TPAMI 2013; TYPE: paper; ACCESS:
  **Free** author PDFs / **Paid** Springer & IEEE
  ([TPAMI PDF](https://people.csail.mit.edu/kaiming/publications/pami12guidedfilter.pdf),
  [DOI 10.1109/TPAMI.2012.213](https://doi.org/10.1109/TPAMI.2012.213)). An O(N) edge-preserving
  filter via a local linear model. The strong, faster alternative to a brute-force bilateral for
  denoise/clarity. *Advanced.*
- **Digital Photography (CS 178)** — Marc Levoy, Stanford; and **Digital & Computational
  Photography (6.815/6.865)** — Frédo Durand, MIT; TYPE: course; ACCESS: **Free**
  ([CS178](https://graphics.stanford.edu/courses/cs178/),
  [Levoy lectures](https://sites.google.com/site/marclevoylectures/home),
  [MIT photo](https://people.csail.mit.edu/fredo/photo.html)). Full courses on image formation,
  optics, tone mapping, and computational-photography pipelines — broad context for the entire
  editing layer. *Intermediate.*

## 8. Dehazing

- **Single Image Haze Removal Using Dark Channel Prior** — He, Sun & Tang, CVPR 2009 (Best
  Paper) / IEEE TPAMI 2011; TYPE: paper; ACCESS: **Free** author PDF / **Paid** IEEE
  ([TPAMI PDF](http://mmlab.ie.cuhk.edu.hk/2011/Haze.pdf),
  [CVPR DOI 10.1109/CVPR.2009.5206515](https://doi.org/10.1109/CVPR.2009.5206515),
  [TPAMI DOI 10.1109/TPAMI.2010.168](https://doi.org/10.1109/TPAMI.2010.168); local copy
  `docs/spatial-dehaze-he-2011-tpami.pdf`). **The** primary source for `latent-cpu::dehaze`:
  patch dark-channel estimation of the veil, then inversion of the scattering model.
  *Intermediate.*
- **Dehazing with Dark Channel Prior: Analysis and Implementation** — Lisani & Hessel, IPOL 2024;
  TYPE: paper + reference code; ACCESS: **Free** (open access)
  ([article](http://www.ipol.im/pub/art/2024/530/),
  [DOI 10.5201/ipol.2024.530](https://doi.org/10.5201/ipol.2024.530); local copy
  `docs/spatial-dehaze-ipol-2024.pdf`). A reproducible, documented implementation of the dark
  channel prior — the implementation-grade companion to (8.6). *Intermediate.*

## 9. Multi-view / projective geometry for homographies & lens models

- **Multiple View Geometry in Computer Vision** — Hartley & Zisserman, 2nd ed., 2004, Cambridge
  University Press; TYPE: book; ACCESS: **Paid** + **Free** sample chapters
  ([authors' site](https://www.robots.ox.ac.uk/~vgg/hzbook/),
  [publisher](https://www.cambridge.org/9780521540513)). The definitive treatment of
  homographies, projective transforms, and camera models — the math behind keystone/perspective
  in `latent`'s geometry stage. *Advanced.*
- **A Flexible New Technique for Camera Calibration** — Zhengyou Zhang, IEEE TPAMI 2000; TYPE:
  paper; ACCESS: **Free** MSR / **Paid** IEEE
  ([MSR](https://www.microsoft.com/en-us/research/publication/a-flexible-new-technique-for-camera-calibration/),
  [DOI 10.1109/34.888718](https://doi.org/10.1109/34.888718)). Planar, homography-based
  intrinsic/extrinsic calibration. Concrete homography estimation grounding for perspective
  correction. *Intermediate.*
- **OpenCV Camera Calibration docs (Brown–Conrady distortion)** — OpenCV; TYPE: official-docs;
  ACCESS: **Free**
  ([calibration tutorial](https://docs.opencv.org/4.x/dc/dbb/tutorial_py_calibration.html),
  [calib3d module](https://docs.opencv.org/4.x/d9/d0c/group__calib3d.html)). The reference
  statement of the radial (k1,k2,k3) + tangential (p1,p2) distortion model. The Brown–Conrady form
  that `latent`'s distortion polynomial is a variant of. *Foundational.*
- **PanoTools Lens correction model** — PanoTools.org wiki; TYPE: website/wiki; ACCESS: **Free**
  ([wiki](https://wiki.panotools.org/Lens_correction_model); local copy
  `docs/geometry-panotools-lens-correction-model.html`). The (a,b,c,d) 3rd-degree radial
  polynomial used across panorama/lens-correction tooling — one of the exact distortion forms
  `latent-lens` maps. *Foundational.*
- **lensfun manual — lens models & calibration data format** — Lensfun project; TYPE:
  official-docs / source-code; ACCESS: **Free/open-source**
  ([calibration format](https://lensfun.github.io/manual/latest/elem_calibration.html),
  [corrections pipeline](https://lensfun.github.io/manual/latest/corrections.html),
  [source](https://github.com/lensfun/lensfun); local copies
  `docs/geometry-lensfun-*.{html,cpp}`). Defines poly3/poly5/ptlens distortion, the vignetting
  model, and TCA — precisely the model `latent-lens` reads from lensfun and `latent-pipeline`
  applies. The single most directly relevant reference for the lens subsystem. *Intermediate.*

## 10. Lens optics & aberrations (vignetting, distortion, chromatic aberration)

- **Applied Photographic Optics** — Sidney F. Ray, 3rd ed., 2002, Focal Press; TYPE: book;
  ACCESS: **Paid** / borrowable
  ([Routledge](https://www.routledge.com/Applied-Photographic-Optics/Ray/p/book/9780240515403),
  [Internet Archive borrow](https://archive.org/details/appliedphotograp0000rays)). The physical
  origin of the corrections `latent` applies: lens aberrations, cos⁴ vignetting falloff, and
  lateral (transverse) chromatic aberration. Explains *why* the lens models look the way they do.
  *Advanced.*
- **lensfun vignetting & TCA model** — Lensfun project; TYPE: official-docs; ACCESS:
  **Free/open-source**
  ([calibration format](https://lensfun.github.io/manual/latest/elem_calibration.html)). The
  concrete polynomial vignetting (k1,k2,k3) and TCA (linear/poly3) coefficients that
  `latent-edit::LensProfile` stores and `latent-pipeline` evaluates per channel. *Intermediate.*

## 11. The engineering of real RAW developers

- **dcraw.c** — Dave Coffin; TYPE: source-code; ACCESS: **Free**
  ([source](https://www.dechifro.org/dcraw/dcraw.c),
  [history mirror](https://github.com/ncruces/dcraw)). The original single-file RAW decoder — a
  primary engineering artifact for how unpacking, black/white levels, white balance scaling, and
  camera matrices are handled in practice. The reference behind LibRaw (and thus `latent-raw`).
  *Advanced.*
- **darktable user manual + source** — darktable project; TYPE: official-docs + source-code;
  ACCESS: **Free/open-source (GPLv3)**
  ([manual](https://docs.darktable.org/usermanual/),
  [source](https://github.com/darktable-org/darktable)). The most architecturally relevant
  open-source developer to study: a modular, scene-referred, ordered pipeline — the closest
  large-scale analogue to `latent`'s design. *Advanced.*
- **RawTherapee documentation (RawPedia) + source** — RawTherapee project; TYPE: official-docs +
  source-code; ACCESS: **Free/open-source (GPLv3)**
  ([RawPedia](https://rawpedia.rawtherapee.com/),
  [source](https://github.com/RawTherapee/RawTherapee)). Excellent documentation of demosaic
  algorithms, tone/color tools, and processing order — a second reference implementation to
  cross-check `latent`'s primitives. *Advanced.*
- **Aurélien Pierre — scene-referred / Filmic RGB / tone-equalizer articles** — TYPE:
  website/blog; ACCESS: **Free**
  ([blog](https://eng.aurelienpierre.com/),
  ["Filmic, darktable and the quest of HDR tone mapping"](https://eng.aurelienpierre.com/2018/11/filmic-darktable-and-the-quest-of-the-hdr-tone-mapping/)).
  The author of darktable's filmic/tone-equalizer modules explaining the modern scene-referred
  workflow philosophy that `latent`'s linear-light, perceptual-domain-curve design follows.
  *Intermediate.*
- **PixInsight documentation** — Pleiades Astrophoto; TYPE: official-docs; ACCESS:
  **Paid/commercial**
  ([reference docs](https://pixinsight.com/doc/),
  [tutorials](https://pixinsight.com/tutorials/)). A rigorous, math-forward commercial raw/astro
  processor; useful for seeing a different, very explicit treatment of linear processing and
  numerical methods. *Advanced.*

## 12. GPU image processing

- **WebGPU specification + WGSL specification** — W3C GPU for the Web WG; TYPE: standard; ACCESS:
  **Free** ([WebGPU](https://www.w3.org/TR/webgpu/), [WGSL](https://www.w3.org/TR/WGSL/)). The
  authoritative definition of the compute/render API and shading language. The spec behind
  `latent-gpu`'s WGSL kernels (`box_blur.wgsl`, `map_pixels.wgsl`, `resample.wgsl`).
  *Intermediate.*
- **wgpu documentation** — gfx-rs project; TYPE: official-docs + source-code; ACCESS:
  **Free/open-source** ([wgpu.rs](https://wgpu.rs/), [docs.rs/wgpu](https://docs.rs/wgpu/),
  [source](https://github.com/gfx-rs/wgpu)). The Rust WebGPU implementation `latent-gpu` is built
  on — the practical API reference for binding buffers/textures and dispatching compute.
  *Intermediate.*
- **GPU Gems 1 / 2 / 3** — NVIDIA (eds. Fernando / Pharr / Nguyen), 2004–2007; TYPE: book; ACCESS:
  **Free** online
  ([GPU Gems](https://developer.nvidia.com/gpugems/gpugems/),
  [GPU Gems 2](https://developer.nvidia.com/gpugems/gpugems2/),
  [GPU Gems 3](https://developer.nvidia.com/gpugems/gpugems3/)). Free, full-text chapters on GPU
  image-processing patterns (separable convolution, tone mapping, post-processing) directly
  transferable to WGSL compute kernels. *Intermediate.*
- **Programming Massively Parallel Processors: A Hands-on Approach** — Hwu, Kirk & El Hajj, 4th
  ed., 2022, Morgan Kaufmann; TYPE: book; ACCESS: **Paid**
  ([Elsevier](https://shop.elsevier.com/books/programming-massively-parallel-processors/hwu/978-0-323-91231-0)).
  GPU architecture and parallel patterns (convolution/stencil are worked examples). The "how to
  think about GPU performance" backing for writing efficient compute kernels. *Advanced.*

---

## If you only read five things

1. **Michael Brown's In-Camera Pipeline tutorial** *(2.5)* — the whole RAW→sRGB map in one sitting (free).
2. **Poynton's Color + Gamma FAQs** *(1.5b)* and **Bruce Lindbloom's site** *(1.6)* — the color-math vocabulary and the exact matrices `color.rs` uses (free).
3. **Malvar–He–Cutler (2004)** *(3.6)* + its **IPOL implementation** *(3.8)* — the demosaic `latent` actually runs (free).
4. **Tomasi–Manduchi bilateral (1998)** *(7.1)* and **He–Sun–Tang dark channel prior** *(8.6)* — the two signature edge-aware primitives (denoise, dehaze) (free).
5. **lensfun model docs** *(9.5 / 10)* + **Hartley & Zisserman** homography chapters *(9.1)* — the geometry & lens-correction math (free docs; book paid, free sample chapters).

---

## Primary sources for the algorithms `latent` actually implements

| `latent` algorithm / feature | Where in code | Canonical source | Link |
|---|---|---|---|
| Malvar–He–Cutler demosaic | `latent-cpu` / `latent-gpu` | Malvar, He, Cutler, *High-Quality Linear Interpolation for Demosaicing of Bayer-Patterned Color Images*, ICASSP 2004 (+ IPOL 2011 ref. impl., DOI 10.5201/ipol.2011.g_mhcd) | [MSR PDF](https://www.microsoft.com/en-us/research/publication/high-quality-linear-interpolation-for-demosaicing-of-bayer-patterned-color-images/) · [IPOL](https://www.ipol.im/pub/art/2011/g_mhcd/) |
| Bilateral denoise | `latent-cpu::denoise` / `bilateral_pixel` | Tomasi & Manduchi, *Bilateral Filtering for Gray and Color Images*, ICCV 1998 | [PDF](https://users.soe.ucsc.edu/~manduchi/Papers/ICCV98.pdf) · [DOI](https://doi.org/10.1109/ICCV.1998.710815) |
| Dark-channel dehaze | `latent-cpu::dehaze` / `dehaze_dark_channel`,`dehaze_recover` | He, Sun, Tang, *Single Image Haze Removal Using Dark Channel Prior*, CVPR 2009 / TPAMI 2011 | [TPAMI PDF](http://mmlab.ie.cuhk.edu.hk/2011/Haze.pdf) · [DOI](https://doi.org/10.1109/TPAMI.2010.168) |
| Brown–Conrady / radial distortion (poly3/poly5/PTLens) | `latent-edit::LensProfile.distortion` → `latent-pipeline::warp` | Brown, *Decentering Distortion of Lenses* (1966); OpenCV calib3d distortion model; PanoTools / lensfun model | [OpenCV](https://docs.opencv.org/4.x/dc/dbb/tutorial_py_calibration.html) · [PanoTools](https://wiki.panotools.org/Lens_correction_model) · [lensfun](https://lensfun.github.io/manual/latest/elem_calibration.html) |
| Lateral chromatic aberration (per-channel radial scale) | `latent-edit::LensProfile.chromatic` → `warp` | lensfun TCA model; Ray, *Applied Photographic Optics* (3rd ed., 2002) | [lensfun](https://lensfun.github.io/manual/latest/elem_calibration.html) · [book](https://www.routledge.com/Applied-Photographic-Optics/Ray/p/book/9780240515403) |
| Vignetting (radial gain) | `latent-edit::LensProfile.vignetting`, `Geometry.vignette` → `apply_radial_gain` | lensfun vignetting model; cos⁴ falloff (Ray, *Applied Photographic Optics*) | [lensfun](https://lensfun.github.io/manual/latest/elem_calibration.html) |
| sRGB color (primaries + transfer function) | `latent-image::color` (`XYZ_TO_LINEAR_SRGB`), `latent-export` | IEC 61966-2-1 (sRGB); SMPTE RP 177 matrix construction; Lindbloom | [Lindbloom](http://www.brucelindbloom.com/) · local `docs/color-srgb-iec61966-2-1-amd1-bgsrgb.pdf` |
| ProPhoto / ROMM RGB working space | `latent-image::color` (`PROPHOTO_PRIMARIES`, D65-pinned) | ISO 22028-2 (ROMM RGB); Lindbloom working-space data | [Lindbloom](http://www.brucelindbloom.com/) · local `docs/color-romm-rgb-iso22028-2.pdf` |
| Camera→XYZ color matrix (from `cam_xyz` / DNG `ColorMatrix`) | `latent-image::color::camera_to_xyz` | Adobe DNG Specification (ColorMatrix); LibRaw `cam_xyz` | [DNG spec](https://helpx.adobe.com/camera-raw/digital-negative.html) · [LibRaw](https://www.libraw.org/docs) |
| Tone curves in a perceptual (gamma) domain | `latent-image::tone` (`apply_linear`) | Poynton, *Gamma FAQ* / *Digital Video and HD* | [Gamma FAQ](https://www.poynton.ca/faq/gammafaq/GammaFAQ.pdf) |
| Unsharp / clarity (midtone-weighted local contrast) | `latent-cpu::combine` + `midtone_weight` | Durand & Dorsey base/detail decomposition, SIGGRAPH 2002 | [MIT page](https://people.csail.mit.edu/fredo/PUBLI/Siggraph2002/) · [DOI](https://doi.org/10.1145/566654.566574) |
| Box / Gaussian blur (separable) | `latent-cpu::blur` / `blur_axis`, `box_blur.wgsl` | Szeliski, *Computer Vision*, 2nd ed., Ch. 3 (linear filtering) | [Book](https://szeliski.org/Book/) · local `docs/geometry-szeliski-03-image-processing.pdf` |
| Homography / keystone / perspective | `latent-edit::Perspective` → `latent-pipeline::warp` | Hartley & Zisserman, *Multiple View Geometry* (homographies); Zhang 2000 | [HZ book](https://www.robots.ox.ac.uk/~vgg/hzbook/) · [Zhang](https://doi.org/10.1109/34.888718) |
| Bilinear resample / inverse-mapping warp | `latent-cpu::sample_bilinear`,`resample`,`warp`, `resample.wgsl` | Heckbert, *Fundamentals of Texture Mapping and Image Warping* (1989); Thévenaz–Blu–Unser | [Heckbert PDF](http://www2.eecs.berkeley.edu/Pubs/TechRpts/1989/Archive/CSD-89-516.pdf) · [TBU PDF](https://bigwww.epfl.ch/publications/thevenaz9901.pdf) |
| GPU compute kernels (WGSL) | `latent-gpu` (`*.wgsl`) | W3C WebGPU + WGSL specs; wgpu docs | [WebGPU](https://www.w3.org/TR/webgpu/) · [WGSL](https://www.w3.org/TR/WGSL/) · [wgpu](https://wgpu.rs/) |

> Note: sRGB (IEC 61966-2-1) and ROMM RGB (ISO 22028-2) standards are paid ISO/IEC documents; the
> repo already caches them under `docs/`, and Bruce Lindbloom's site reproduces the derived
> matrices and chromaticities for free. Brown's original 1966/1971 distortion papers have no
> single canonical free PDF — use the OpenCV / PanoTools / lensfun model pages as the working
> references for the math.

---

## Resources I could not fully verify (caveats)

Every resource above was confirmed to exist with a working canonical link. The following are
**access/availability caveats**, not "could not verify" — flagged for honesty:

- **Bruce Lindbloom's site** (1.6, and the sRGB/ProPhoto table entries): live but only over
  **`http://`** — its HTTPS endpoint has a broken/expired TLS certificate. Use the http URL.
- **Fairchild author page** (`markfairchild.org`): loads but its HTTPS cert is expired; the Wiley
  publisher link is the reliable route.
- **Brown (1966 / 1971) original distortion papers** (9.3 / 10): no single canonical *free* PDF of
  the original *Photogrammetric Engineering* articles. Volume/page numbers confirmed via multiple
  secondary citations; the **OpenCV / PanoTools / lensfun** model docs are the reliable free
  statement of the model. (Listed as a caveat, not a fabrication.)
- **Zhang 2000 DOI** (9.2): the DOI `10.1109/34.888718` is confirmed via the IEEE catalog but is
  **not printed on the Microsoft Research landing page** itself; both URLs are valid.
- **Reinhard 2002 tone-mapping DOI** (5.2): correct DOI is `10.1145/566570.566575` (Crossref-
  confirmed); a `…566654…` variant appears in some search results and is **wrong** — corrected here.
- **Szeliski 2nd-ed PDF** (6.7): free but **gated/personal-use** — link the book page, do not
  repost the PDF.
- **dcraw home page** (`cybercom.net/~dcoffin`): the original author URL has been intermittently
  down for years; the `dechifro.org` copy and the `ncruces/dcraw` GitHub history mirror are the
  reliable archives (used in the links above).
- **DNG version** (2.3): a newer **DNG 1.7 / 1.7.1** (HDR / JPEG-XL) exists; **1.6.0.0** is the
  version on Adobe's current canonical page and matches the repo's cached spec.
