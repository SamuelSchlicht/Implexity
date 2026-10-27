# Implexity

A Rust framework for simultaneous multiphysics topology optimization with differentiable implicit geometry and material ageing.

<picture>
  <source media="(max-width: 600px)" srcset="docs/assets/geometry-mobile.png">
  <img src="docs/assets/geometry.png" alt="A periodic TPMS morphs through spatially varying geometry into a cooled component with integrated passages and a partially exposed interior." width="100%">
</picture>

*Illustrative morph from a periodic TPMS to a cooled component, generated with Implexity.*

<details>
<summary>Geometry morph</summary>

![Continuous geometry morph through Implexity’s spatial controls and implicit geometry operations.](docs/assets/geometry-morph.gif)

</details>

## Formulation

The formulation combines an arbitrary number of compatible physics models in a shared optimization problem. Objectives and constraints account for their couplings and ageing-dependent material response.

Phase potentials, thickness fields, topology masks, and material allocation define the geometry. Automatic differentiation and adjoints return sensitivities through the coupled physics and material history to these controls.

[Method](docs/method.md) · [Research notes](docs/research.md)

## Research

### Differentiable scaffold geometries

[Direct gradient-based topology optimization of differentiable scaffold geometries under physicochemical ageing](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7350458)

Joint optimization of lattice geometry under thermoelastic loading, physicochemical ageing, and surrogate tissue deposition and resorption.

<picture>
  <source media="(max-width: 600px)" srcset="docs/assets/scaffold-study-mobile.png">
  <img src="docs/assets/scaffold-study.png" alt="Unheated scaffold designs for a symmetry-edge point load, a half-span point load, and a distributed top load." width="100%">
</picture>

*Unheated designs under different loading conditions.*

<details>
<summary>Optimization sequence</summary>

![The reported unheated point-load optimization sequence, assembled from the manuscript’s geometry panels.](docs/assets/scaffold-evolution.gif)

</details>

### Liquid-cooled divertor architectures

[Lifecycle-aware topology optimization of liquid-cooled fusion reactor divertors](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7596080)

Joint thermal, hydraulic, and mechanical design of tungsten–CuCrZr architectures with creep and helium retention.

<picture>
  <source media="(max-width: 600px)" srcset="docs/assets/divertor-study-mobile.png">
  <img src="docs/assets/divertor-study.png" alt="Initial and optimized ageing-aware divertor geometries, with solid half-sections above and complete coolant volumes below." width="100%">
</picture>

## Getting started

Build with the repository’s [Rust toolchain](rust-toolchain.toml).

```sh
git clone https://github.com/SamuelSchlicht/Implexity.git
cd Implexity
cargo build --release --locked -p implexity-cli --bin implexity --no-default-features
./target/release/implexity model examples --copy models
```

[Installation and usage](docs/getting-started.md)

## Graphical workbench

```sh
./target/release/implexity serve --workspace ./workspace
```

Open the [local workbench](http://127.0.0.1:8765). The workflow runs from geometry authoring through coupled physics and ageing to optimization and inspection.

![Implexity’s Geometry workspace showing the bundled model, model tree, viewport tools, and parameter controls.](docs/assets/gui-geometry.png)

| Workspace | Use |
| :--- | :--- |
| Geometry | Create or import geometry; edit parameters, fields, and design bounds. |
| Conditions | Select regions and assign loads and boundary conditions. |
| Physics | Activate compatible add-ins; configure materials, couplings, and ageing laws. |
| Objectives | Combine response objectives and constraints. |
| Optimise | Check readiness, run, pause, and resume optimization. |
| Results | Inspect recorded fields, histories, and sensitivities. |

<details>
<summary>Physics and ageing setup</summary>

![Implexity’s Physics workspace with coupled-history and environmental-ageing add-ins activated.](docs/assets/gui-physics.png)

Use **Manage physics add-ins** to select components, then open the provider setup to define their material and history data.

</details>

Use **Output** for mesh export, images, and sections. [GUI walkthrough](docs/gui.md)

## Repository structure

| Area | Modules |
| :--- | :--- |
| Geometry | [Implicit kernel](crates/implexity-geometry) · [Lattice representation](crates/implexity-geometry/src/lattice) · [Example models](crates/implexity-geometry/data/models) |
| Derivatives and numerics | [Automatic differentiation](crates/implexity-ad) · [Linear algebra](crates/implexity-linalg) · [Solvers](crates/implexity-solve) |
| Physical models | [Solids](crates/implexity-physics-solid) · [CFD](crates/implexity-physics-cfd) · [Thermofluids](crates/implexity-physics-thermofluid) · [Fields](crates/implexity-physics-fields) |
| Optimization and execution | [Optimization](crates/implexity-optim) · [Physics orchestration](crates/implexity-core/src/orchestration) · [Runtime](crates/implexity-runtime) · [Jobs](crates/implexity-jobs) |
| Inspection and export | [Meshing](crates/implexity-mesh) · [Rendering](crates/implexity-render) · [Browser viewer](viewer) |
| Interfaces | [CLI](crates/implexity-cli) · [Server](crates/implexity-server) · [MCP bridge](crates/implexity-mcp) · [Desktop workbench](crates/implexity-desktop) |

## Attribution and license

Samuel Schlicht · Metaplexis Ingenieurgesellschaft mbH

[Papers](#research) · [BibTeX](docs/references.bib) · [Apache License](LICENSE) · [Disclaimer](DISCLAIMER.md) · [Third-party notices](THIRD_PARTY_NOTICES.md)
