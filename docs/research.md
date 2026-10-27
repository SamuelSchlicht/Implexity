# Research and figure provenance

The research examples associated with the repository are based on the scaffold and divertor manuscripts described below. Both studies consider the geometric dependence of the physical response under the influence of an evolving material state, although their geometric parameterizations, their physical models, and their numerical procedures differ. The repository revision and the figure-generation records are identified in [verification.json](frontpage/verification.json). The study figures reproduce the reported geometries from the manuscripts; their preparation did not involve a repetition of the underlying optimization campaigns.

## Scaffold study

Samuel Schlicht, *Direct gradient-based topology optimization of differentiable scaffold geometries under physicochemical ageing*. [Manuscript](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7350458).

The scaffold formulation retains an implicit lattice throughout the optimization, with the phase potentials, the wall-thickness field, and the macroscopic topology mask defining the geometric design. The resulting smooth material fields connect the lattice to the resolved mechanical problem and to the selected exposure or history model. The considered mechanisms comprise the thermoelastic loading, the cyclic deterioration, the geometry-dependent oxidative transport, the prescribed photolytic exposure, and the surrogate descriptions of tissue deposition and resorption.

The relevance of the geometry extends beyond its influence on the initial stiffness. The wall thickness, the pore arrangement, and the distribution of the load paths also affect the conditions under which the local material properties evolve. Where the corresponding dependency is included in the selected exposure or history law, the sensitivity of the final response is returned through that evolution to the geometric design variables.

The study distinguishes the following comparisons according to the physical or numerical dependency being examined:

- **Load-dependent geometry.** The unheated scaffold examples characterize the adaptation of the lattice architecture to the respective loading conditions, through which the influence of the load placement and distribution can be examined.
- **Deterioration-aware continuation.** The geometric continuations obtained under the consideration of deterioration are evaluated against the corresponding reference designs obtained without that consideration.
- **History sensitivity.** A separate derivative comparison examines the complete sensitivity of the coupled formulation against a sensitivity evaluation in which the material state is held fixed.
- **Geometry extraction.** The relaxed objectives are compared with the responses of volume-matched binary geometries, through which the influence of the extraction and the analysis resolution is assessed.

The deterioration-aware continuation and the history-sensitivity comparison address different aspects of the formulation. The former includes the influence of deterioration on the continued design process, whereas the latter examines the derivative contribution transmitted through the evolving material state. Likewise, an improvement in the relaxed objective does not determine the magnitude of the improvement after binary extraction. The interpretation of these comparisons therefore depends on the interface penalties, the spatial and temporal resolutions, and the idealized material and biological laws used in the study.

### Source images

[scaffold-study.png](assets/scaffold-study.png) contains the **unheated column of Figure 6**, comprising the symmetry-edge point load, the half-span point load, and the distributed top load. The displayed panels retain the source geometry and the grayscale occupancy, while the labels identify the respective loading conditions. These examples concern the load-dependent lattice adaptation; the binary deterioration benchmark is a separate comparison.

[scaffold-evolution.gif](assets/scaffold-evolution.gif) presents the existing geometry panels of **Figure 8** in their source order. The animation uses the reported states without the synthesis of intermediate geometries. The playback duration and the progress indicator specify the presentation of that sequence and have no correspondence to the computational duration of an optimization step.

## Divertor study

Samuel Schlicht, *Lifecycle-aware topology optimization of liquid-cooled fusion reactor divertors*. [Manuscript](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7596080).

The divertor formulation couples an implicit tungsten–CuCrZr architecture to the incompressible coolant flow, the conjugate heat transfer, the thermoelastic response, and the material history. The primary ageing comparison includes Norton creep and helium retention, through which the operating fields influence the material properties entering the aged response. The effects of displacement damage and of the applied pressure variation are examined in separate campaigns.

For each design in the primary comparison, a non-aged full-load solution supplies the reference temperature and the reference total strain. These fields remain fixed during the subsequent history increment, after which the aged full-load response is evaluated. The design derivative includes the reference solution, the history map, and the aged solution. The geometric dependence of the aged response is thus retained through that sequence, with the reference fields held constant within the history increment.

The comparison of the resulting designs is conducted under common evaluation conditions. The peak temperatures used for the characterization of the designs are distinguished from the smooth thermal response used as the optimization objective. The reported geometries are obtained within finite optimization budgets and retain residual violations of the soft response limits. Their interpretation consequently requires the applied penalties and the achieved response values to be considered together.

The physical formulation uses idealized property and history laws, a fixed reference hydraulic diameter for the wall-film exchange, and a non-deformed flow geometry. The reported investigation establishes the numerical behavior of the considered design formulation under these assumptions; experimental divertor qualification and the demonstration of a corresponding manufacturing process are outside its scope.

### Source images

[divertor-study.png](assets/divertor-study.png) combines the **initial ageing-included solid and coolant panels of Figure 1** with the **final reported ageing-included panels of Figure 2**. The solid half-sections and the complete coolant volumes are arranged in corresponding rows. The dark gray denotes tungsten, the light gray denotes CuCrZr, and the blue denotes the coolant.

The source geometry and the material colors are retained in the comparison. The displayed sequence relates the initial architecture to the reported design obtained under the consideration of ageing. It does not constitute a comparison between an ageing-aware and an ageing-blind design, and the displayed endpoint retains the numerical status of the reported optimization state, including its residual soft-limit violations.

## Illustrative geometry

[geometry.png](assets/geometry.png) presents the variation of a periodic gyroid structure towards a branching cooled manifold. The comparison states are defined through the spatial controls in [morph_models.py](frontpage/source/morph_models.py), exported as meshes by the native Implexity geometry CLI, and rendered from a common camera position. The [geometry animation](assets/geometry-morph.gif) follows the corresponding sequence of native geometry exports.

The local controls vary the phase, the stretch, the TPMS basis composition, the wall thickness, the occupancy, the sheet/network blend, and the residual field. The implicit geometry operations additionally modify the outer envelope and introduce the curved coolant passages and the collars surrounding the ports. The resulting sequence therefore combines the local modification of the lattice with the development of an application-oriented macroscopic geometry.

The control values in this sequence are prescribed. The meshes are computed by the Implexity geometry engine, while the sequence itself is a geometric demonstration without a coupled physical solve or a newly executed optimization campaign. No AI image-generation procedure is used. The render-only smoothing affects the displayed surface finish, and the neutral shading represents the geometric shape without encoding a simulated temperature, stress, or flow field.

The earlier periodic specimen remains available through [hero-model.json](frontpage/source/hero-model.json) and its archived rendering sources. It is separate from the current manifold comparison.

The [authoring guide](frontpage/README.md) documents the generation and the reproduction of the figures. The [extraction manifest](frontpage/source/inputs/provenance.json) records the manuscript hashes, the panel locations, and the source sequence indices. The original study geometry is credited to Samuel Schlicht / Metaplexis and to the respective manuscripts; the repository software license does not alter the rights applying to those manuscripts or their figures.
