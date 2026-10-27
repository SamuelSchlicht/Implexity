# Implexity

Implexity is a Rust framework for physics-driven topology optimization in which the differentiable implicit geometry, the coupled physical response, and the evolving material state are considered within a common design formulation. The geometric adaptation is thereby governed by the application-specific physical conditions and by the material properties that emerge under the consideration of ageing.

<picture>
  <source media="(max-width: 600px)" srcset="docs/assets/geometry-mobile.png">
  <img src="docs/assets/geometry.png" alt="A regular TPMS develops into a branching cooled manifold as the envelope, the cell geometry, the wall thickness, and the coolant passages change." width="100%">
</picture>

*Epoch-wise variation of periodic TPMS structures towards application-adapted structures.*

<details>
<summary>Geometry morph</summary>

![Continuous variation of the geometry through the spatial controls and the implicit geometry operations.](docs/assets/geometry-morph.gif)

</details>

## Formulation

A design can be optimized across any number of compatible physics models at once, which includes arbitrary multi-physical couplings and, in particular, the effects of geometry- and material-dependent ageing.

Locally emerging fields that result from local physical conditions allow to control the cell shape, the cell spacing, the wall thickness, the topology, and the material distribution. Through automatic differentiation and adjoints, these controls are linked to the specific physical response and the dynamically varying ageing history.

[Method](docs/method.md) · [Research notes](docs/research.md)

## Research

### Differentiable scaffold geometries

[Direct gradient-based topology optimization of differentiable scaffold geometries under physicochemical ageing](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7350458)

The scaffold study considers the direct optimization of differentiable lattice geometries under thermoelastic loading and physicochemical ageing. The local wall thickness, the lattice phase, and the macroscopic topology are varied under the consideration of the evolving material state, which includes surrogate descriptions of the tissue deposition and the tissue resorption. The resulting geometric adaptation therefore accounts for the interaction between the structural response and the application-dependent changes in the local material properties.

<picture>
  <source media="(max-width: 600px)" srcset="docs/assets/scaffold-study-mobile.png">
  <img src="docs/assets/scaffold-study.png" alt="Unheated scaffold designs for a symmetry-edge point load, a half-span point load, and a distributed top load." width="100%">
</picture>

*Unheated designs under different loading conditions.*

<details>
<summary>Optimization sequence</summary>

![The unheated point-load optimization sequence.](docs/assets/scaffold-evolution.gif)

</details>

### Liquid-cooled divertor architectures

[Lifecycle-aware topology optimization of liquid-cooled fusion reactor divertors](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7596080)

The divertor study considers the optimization of implicit tungsten–CuCrZr architectures through the simultaneous consideration of the heat transfer, the coolant flow, and the mechanical response. The effects of Norton creep and helium retention are incorporated into the material history, through which the geometric dependence of the aged thermophysical response contributes to the design gradient. The resulting adaptation concerns both the solid architecture and the corresponding coolant domain.

<picture>
  <source media="(max-width: 600px)" srcset="docs/assets/divertor-study-mobile.png">
  <img src="docs/assets/divertor-study.png" alt="The initial and optimized ageing-aware divertor geometries, with the solid half-sections above and the complete coolant volumes below." width="100%">
</picture>

## Getting started

The command-line application is built from the repository using the specified [Rust toolchain](rust-toolchain.toml). The following commands compile the application and copy the bundled model documents into a local directory, from which the implicit geometry can be inspected and exported.

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

The [local workbench](http://127.0.0.1:8765) provides access to the geometric description, the physical setup, and the optimization controls within a common graphical interface. The geometry is defined or imported before the physical conditions, the material-history models, and the optimization responses are assigned to the corresponding design problem.

![The Geometry workspace with the model tree, the viewport tools, and the parameter controls.](docs/assets/gui-geometry.png)

| Workspace | Use |
| :--- | :--- |
| Geometry | Define or import the geometry and specify the parameters, the spatial control fields, and the admissible design bounds. |
| Conditions | Select the geometric regions to which the loads and the boundary conditions are to be assigned. |
| Physics | Specify the physical models, the material descriptions, the inter-model couplings, and the applicable ageing laws. |
| Objectives | Assign the evaluated responses to the optimization objectives and to the associated response bounds. |
| Optimise | Review the readiness of the configured problem and control the execution, the interruption, or the continuation of the optimization. |
| Results | Inspect the recorded physical fields, the material histories, and the design sensitivities associated with the evaluated states. |

<details>
<summary>Physics and ageing setup</summary>

![The Physics workspace with the coupled-history and the environmental-ageing add-ins activated.](docs/assets/gui-physics.png)

The required physical components are activated through **Manage physics add-ins**. The corresponding provider setup is then used to specify the material data and the history data through which the selected components are incorporated into the coupled problem.

</details>

The **Output** menu provides access to the export of the meshes, the rendered images, and the geometric sections. The complete sequence of the geometric definition, the physical setup, and the result inspection is described in the [GUI walkthrough](docs/gui.md).

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
