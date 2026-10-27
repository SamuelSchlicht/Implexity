# Front-page figures and reproduction

The repository front page is implemented through standard GitHub Markdown, relative asset paths, and optional animated sequences. The figures are embedded as raster assets, while the headings, the captions, and the descriptive text retain the native Markdown presentation. The HTML files in `source` define editable figure layouts that are rasterized before publication, so that the displayed page does not require custom stylesheets, executable scripts, or external font requests.

## Geometric representation and presentation

The opening figure follows the variation of a periodic TPMS towards a branching cooled manifold. Each state is evaluated and exported through the native Implexity geometry CLI, after which the corresponding mesh is rendered in Blender from a common camera position. The spatial controls vary the phase, the stretch, the TPMS basis composition, the wall thickness, the occupancy, the sheet/network blend, and the residual field. The outer envelope is modified around the curved coolant passages, with collars introduced at the ports.

The control values are prescribed for the purpose of demonstrating the geometric representation. The sequence therefore documents the native geometric evaluation without constituting a newly executed physical optimization. The research figures, by comparison, reproduce the geometry and the material colors contained in the reported manuscript panels.

The figures use a white background and neutral geometric shading in both GitHub themes. The captions and the descriptions are provided through Markdown, while standard picture elements select rearranged layouts at narrow display widths. The animated sequences are placed within collapsed disclosures, through which their playback can be accessed without extending the initially displayed document.

## Source files

| File | Role |
| :--- | :--- |
| `source/morph_models.py` | Defines the prescribed spatial controls and the implicit geometry operations for the manifold sequence. |
| `source/render_morph.py` | Renders the native exported meshes from a common camera position. |
| `source/morph_animation.py` | Assembles the rendered native states into the animated geometry sequence. |
| `source/hero-model.json` | Retains the earlier periodic specimen derived from the bundled lattice-block example. |
| `source/render_scene.py` | Specifies the material, the illumination, the camera, and the render-only smoothing for the archived specimen. |
| `source/section_svg.py` | Computes the central planar intersection of the archived exported mesh. |
| `source/prepare_figures.py` | Extracts the manuscript panels and assembles the reported scaffold sequence. |
| `source/layouts.py` | Defines the arrangement and the labeling of the static figure panels. |
| `source/render_assets.mjs` | Rasterizes the figure layouts through the browser rendering engine. |
| `source/inputs/provenance.json` | Records the manuscript hashes, the source figures, and the panel coordinates. |
| `verification.json` | Records the source revision and the toolchain, export, and presentation checks. |

The published assets are stored in `../assets`. The extracted manuscript panels and the rendered geometry inputs are retained in `source/inputs`, which permits the static layouts to be regenerated from those retained inputs without repeating the manuscript extraction or the mesh rendering.

## Rebuild the geometry morph

The comparison is reconstructed by generating the model documents, evaluating each document through the native geometry CLI, and rendering the resulting meshes:

```sh
python docs/frontpage/source/morph_models.py morph-states --fine
./target/release/implexity model export morph-states/morph-00.json --spacing 0.10 -o morph-states/morph-00.stl --json
./target/release/implexity model export morph-states/morph-01.json --spacing 0.10 -o morph-states/morph-01.stl --json
./target/release/implexity model export morph-states/morph-02.json --spacing 0.10 -o morph-states/morph-02.stl --json
blender -b --python docs/frontpage/source/render_morph.py -- morph-states
```

The resulting PNG files are copied into `docs/frontpage/source/inputs`, after which the layout commands below regenerate the static comparison. NumPy is required for the authoring of the spatial control values.

For the animation, the model documents are generated with `--frames 25`, exported at `--spacing 0.14`, and rendered as a sequence. The command `morph_animation.py frames-folder geometry-morph.gif` then assembles those renders through Pillow. The forward and reverse playback follows the prescribed control sequence; its timing is a presentation setting and does not represent the duration or the convergence of an optimization process.

The neutral material appearance and the render-only smoothing are used to display the geometry. They do not encode a simulated physical field, and no AI image-generation procedure forms part of the mesh generation, the rendering, or the animation assembly.

## Archived periodic specimen

The earlier periodic specimen is retained separately from the current manifold sequence. With the CLI built, its mesh, rendered view, and central section are reproduced from the repository root through:

```sh
./target/release/implexity model export docs/frontpage/source/hero-model.json --spacing 0.08 -o specimen.stl --json
blender -b --python docs/frontpage/source/render_scene.py -- specimen.stl docs/frontpage/source/inputs/specimen.png 128
python docs/frontpage/source/section_svg.py specimen.stl docs/frontpage/source/inputs/section.svg
```

The section calculation requires `trimesh` and `numpy`. The exported mesh is intersected with its central horizontal plane, after which the closure of the resulting contours is checked and the corresponding filled cross-section is drawn. The section therefore follows directly from the exported geometry.

The Blender source applies a small smoothing modifier to the rendered surface. The material and the illumination remain neutral, and the scene is saved beside the render to permit inspection of the rendering configuration. The scene and the STL are intermediate reproduction files; the export and render settings required to reconstruct them are retained in the source and the verification record.

## Rebuild the research figures

The manuscript extraction requires `pymupdf` and `Pillow` in the Python environment. A fresh extraction is conducted with the manuscript versions from which the retained panels were obtained:

```sh
python docs/frontpage/source/prepare_figures.py --scaffold /path/to/ssrn-7350458.pdf --fusion /path/to/ssrn-7596080.pdf
```

The extraction script checks the dimensions of the embedded images before applying the recorded crop coordinates. Where a revised manuscript changes the page arrangement or the embedded image dimensions, the corresponding page references and crop coordinates require review. The extracted geometry and the material colors are retained, while the panel arrangement, the labels, and the animation timing are defined by the presentation sources.

The static layouts and their raster assets are regenerated through:

```sh
python docs/frontpage/source/layouts.py
cd docs/frontpage/source
npm install --no-save playwright
npx playwright install chromium
node render_assets.mjs
```

The browser rendering waits for the images and the fonts to become available before the figure is captured. An existing Playwright installation can be selected through `PLAYWRIGHT_MODULE`, while `PLAYWRIGHT_BROWSERS_PATH` specifies the location of the browser cache.

## Attribution

The GUI figures are unretouched screenshots of the running workbench with the bundled geometry example. The Physics view records the add-in selection with the coupled-history and the environmental-ageing packages active. The screenshots document the interface configuration and do not contain the result of a completed physical solve or optimization campaign.

The static layouts use the platform's Arial or Helvetica font. Manrope and IBM Plex Mono are bundled for the animation, together with their SIL Open Font License texts in `source/fonts`, from the [Google Fonts repository](https://github.com/google/fonts).

The original study geometry is credited to Samuel Schlicht / Metaplexis and to the corresponding manuscripts. The software distribution does not alter the rights applying to the papers or their figures. The illustrative model retains the adjacent license notice of the original example, while the authoring scripts carry Apache-2.0 SPDX identifiers. The repository license and the third-party notices continue to apply to their respective components.

The scientific scope and the identification of the manuscript panels are described in the [research notes](../research.md).
