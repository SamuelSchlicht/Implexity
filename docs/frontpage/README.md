# Front-page figures and reproduction

The repository front page uses standard GitHub Markdown, relative image paths, and an optional animation. Its presentation does not depend on custom CSS, JavaScript, external font requests, or badge services. HTML files in `source` are editable figure layouts that are rasterized for GitHub.

## Presentation

The page uses native headings and technical prose. Figures have a white background, neutral typography, and labels describing the geometry. There are no promotional headlines, performance callouts, or numbered feature panels.

The opening figure follows a regular TPMS as it becomes a branching cooled manifold. Implexity exports every state, and Blender renders them from a fixed camera. Local controls change the phase, stretch, TPMS basis, thickness, occupancy, sheet/network blend, and residual field. The outer envelope narrows around curved coolant passages, with collars at the ports. These controls are prescribed to demonstrate the geometry; this is not a physical optimization run. The research figures retain the geometry and colors from the papers.

Figures remain on white in both GitHub themes. Their captions and descriptions are native Markdown. Standard picture elements select rearranged versions at narrow widths so figure labels remain readable. The source-frame animation is inside a collapsed disclosure.

## Source files

| File | Role |
| :--- | :--- |
| `source/morph_models.py` | Reproducible native spatial-control and CSG morph states |
| `source/render_morph.py` | Fixed-camera technical rendering of the exported meshes |
| `source/morph_animation.py` | Animation of the native geometry states |
| `source/hero-model.json` | Illustrative model derived from the bundled lattice block |
| `source/render_scene.py` | Neutral Blender material, lighting, camera, and render-only smoothing |
| `source/section_svg.py` | Central planar intersection of the exported mesh |
| `source/prepare_figures.py` | Manuscript panel extraction and source-frame animation |
| `source/layouts.py` | Editable technical figure layouts |
| `source/render_assets.mjs` | Browser rasterization of the layouts |
| `source/inputs/provenance.json` | Manuscript hashes, figure references, and panel coordinates |
| `verification.json` | Source, toolchain, export, and presentation verification record |

The final assets live in `../assets`. Extracted panels and the rendered specimen are retained in `source/inputs` so the layouts can be regenerated without the PDFs or Blender.

## Rebuild the geometry morph

Generate the three comparison states, export each with the native CLI, and render them:

```sh
python docs/frontpage/source/morph_models.py morph-states --fine
./target/release/implexity model export morph-states/morph-00.json --spacing 0.10 -o morph-states/morph-00.stl --json
./target/release/implexity model export morph-states/morph-01.json --spacing 0.10 -o morph-states/morph-01.stl --json
./target/release/implexity model export morph-states/morph-02.json --spacing 0.10 -o morph-states/morph-02.stl --json
blender -b --python docs/frontpage/source/render_morph.py -- morph-states
```

Copy the three PNG renders to `source/inputs` and run the layout commands below. NumPy is required for control authoring. For the animation, generate states with `--frames 25`, export them at `--spacing 0.14`, render the sequence, then run `morph_animation.py frames-folder geometry-morph.gif`. Pillow assembles the renders. Forward/reverse playback depicts a prescribed geometry morph, not optimization iterations. The figure uses neutral material appearance; it contains no simulated physical field or performance claim.

## Archived periodic specimen

From the repository root, with the CLI built:

```sh
./target/release/implexity model export docs/frontpage/source/hero-model.json --spacing 0.08 -o specimen.stl --json
blender -b --python docs/frontpage/source/render_scene.py -- specimen.stl docs/frontpage/source/inputs/specimen.png 128
python docs/frontpage/source/section_svg.py specimen.stl docs/frontpage/source/inputs/section.svg
```

The section script requires `trimesh` and `numpy`. It intersects the actual exported mesh with its central horizontal plane, verifies that the contours are closed, and draws their filled cross-section. It does not fit or synthesize a replacement lattice.

The Blender source applies a small smoothing modifier for rendering only. Material and lighting are neutral and do not encode a physical field. The script saves its scene beside the render for inspection; the scene and STL are not required in the repository. Exact export and render settings are retained in the source and verification record.

## Rebuild the research figures

Install `pymupdf` and `Pillow` in a Python environment. For a fresh extraction, provide the author-supplied manuscript versions:

```sh
python docs/frontpage/source/prepare_figures.py --scaffold /path/to/ssrn-7350458.pdf --fusion /path/to/ssrn-7596080.pdf
```

The extraction script checks embedded image dimensions before cropping. A revised manuscript may require reviewing the page references and crop coordinates. Geometry and material colors are retained; labels, arrangement, and playback timing are presentation choices.

Regenerate layouts and raster images:

```sh
python docs/frontpage/source/layouts.py
cd docs/frontpage/source
npm install --no-save playwright
npx playwright install chromium
node render_assets.mjs
```

Browser rasterization waits for images and fonts before capture. `PLAYWRIGHT_MODULE` can specify an existing Playwright module URL, and `PLAYWRIGHT_BROWSERS_PATH` can select its browser cache.

## Attribution

The GUI figures are unretouched screenshots of the running workbench, using its bundled geometry example. The Physics view shows add-in selection, with coupled-history and environmental-ageing packages active. These are interface illustrations, without a completed physical solve or optimization campaign.

The layouts use the platform's Arial or Helvetica font. Manrope and IBM Plex Mono are bundled for the animation, with their SIL Open Font License texts in `source/fonts`, from the [Google Fonts repository](https://github.com/google/fonts).

Original study geometry is credited to Samuel Schlicht / Metaplexis and the corresponding manuscripts. This package does not relicense the papers or their figures. The illustrative model retains the original example's adjacent license notice. New authoring scripts carry Apache-2.0 SPDX identifiers, and repository license and third-party notices remain in force.

See [research.md](../research.md) for scientific scope and figure provenance.
