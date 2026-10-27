# Research and figure provenance

The front page draws on the author-supplied manuscripts below and the repository revision recorded in [verification.json](frontpage/verification.json). The study images reproduce reported geometry; the paper optimization campaigns were not rerun to prepare this page.

## Scaffold study

Samuel Schlicht, *Direct gradient-based topology optimization of differentiable scaffold geometries under physicochemical ageing*. [Manuscript](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7350458).

The formulation retains an implicit lattice throughout optimization. Phase potentials, thickness, and a macroscopic mask describe the design. Smooth material fields connect that geometry to resolved mechanics and the selected exposure or history model. The study includes thermoelastic loading, cyclic deterioration, geometry-dependent oxidative transport, prescribed photolytic exposure, and surrogate tissue deposition and resorption.

The paper separates several comparisons that should be interpreted individually:

- **Load-dependent geometry.** The unheated scaffold examples illustrate how different loading conditions change the lattice architecture.
- **Deterioration-aware continuation.** Designs continued with deterioration included are compared with their corresponding blind reference designs.
- **History sensitivity.** A separate comparison evaluates full derivatives against derivatives with the material state frozen.
- **Geometry extraction.** Relaxed objectives are compared with volume-matched binary evaluations; the results depend on extraction and analysis resolution.

The deterioration-aware continuation comparison does not isolate the effect of the state-mediated derivative. Improvements in the relaxed formulation also need not carry over unchanged to extracted geometry. The manuscript discusses interface penalties, spatial and temporal resolution, and the limitations of idealized material and biological laws.

### Source images

[scaffold-study.png](assets/scaffold-study.png) reproduces the **unheated column of Figure 6**, with plain labels for the symmetry-edge point load, half-span point load, and distributed top load. The panels retain the source geometry and grayscale occupancy. They are distinct from the binary deterioration benchmark.

[scaffold-evolution.gif](assets/scaffold-evolution.gif) sequences the existing geometry panels of **Figure 8**. No intermediate states are synthesized. Playback durations and the progress indicator describe the presentation sequence, not computation time. The animation is optional on the front page.

## Divertor study

Samuel Schlicht, *Lifecycle-aware topology optimization of liquid-cooled fusion reactor divertors*. [Manuscript](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7596080).

The formulation couples an implicit tungsten–CuCrZr architecture to incompressible coolant flow, conjugate heat transfer, thermoelasticity, and material history. The primary ageing comparison includes Norton creep and helium retention. Separate campaigns examine displacement damage and pressure variation.

A non-aged full-load solution supplies reference temperature and total strain. These reference fields remain fixed during the history increment, after which an aged full-load response is evaluated. The derivative includes the reference solve, history map, and aged solve. This is a specific history approximation, rather than continuous recalculation of all reference fields during ageing.

The manuscript compares designs under common evaluation conditions and distinguishes peak temperatures from the smooth thermal objective used for optimization. It reports finite-budget penalized designs with residual soft-limit violations. Other modeling assumptions include idealized property and history laws, a fixed reference hydraulic diameter for wall-film exchange, and a non-deformed flow geometry. The study is a numerical investigation, without experimental divertor qualification or a demonstrated manufacturing process.

### Source images

[divertor-study.png](assets/divertor-study.png) combines the **initial ageing-included solid and coolant panels of Figure 1** with the **final reported ageing-included panels of Figure 2**. Solid half-sections and complete coolant volumes are arranged in corresponding rows. Dark gray indicates tungsten, light gray indicates CuCrZr, and blue indicates coolant.

Geometry pixels and material colors are retained. The comparison depicts the initial architecture and a design obtained with ageing included; it does not depict an ageing-aware versus ageing-blind pair. The figure represents a reported optimization state, not a claim of converged constraint satisfaction.

## Illustrative geometry

[geometry.png](assets/geometry.png) shows a gyroid sheet exported by the Implexity geometry CLI from [hero-model.json](frontpage/source/hero-model.json), derived from the bundled lattice-block example. The left view is a central planar intersection of the exported mesh. The right view is an orthographic rendering of that same mesh with neutral illumination.

Render-only smoothing softens sampling-scale surface ripples. The exported geometry and its topology record remain unchanged. Surface shading represents shape; it does not encode a mechanical, thermal, flow, or optimization result.

The [authoring guide](frontpage/README.md) records the reproducible workflow. The [extraction manifest](frontpage/source/inputs/provenance.json) retains manuscript hashes, panel locations, and source sequence indices. Original study geometry is credited to Samuel Schlicht / Metaplexis and the respective papers. The software license does not relicense the manuscripts or their figures.
