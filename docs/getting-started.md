# Installation and initial model evaluation

The command-line geometry workflow provides a direct route from the source checkout to the evaluation of an implicit model and the export of its surface mesh. The following procedure establishes that workflow before introducing the graphical workbench and the study interfaces, through which the corresponding model definition and the physical setup can be developed further.

## Build the CLI

The compilation requires Rust, installed through [rustup](https://rustup.rs/), together with the native linker for the selected platform. On Windows, the MSVC Rust toolchain requires the Visual Studio C++ Build Tools and a Windows SDK. On Linux or macOS, the corresponding native compiler toolchain provides the required linking environment.

```sh
git clone https://github.com/SamuelSchlicht/Implexity.git
cd Implexity
cargo build --release --locked -p implexity-cli --bin implexity --no-default-features
```

The repository specifies Rust 1.98.1 in `rust-toolchain.toml`, while the workspace declares a minimum Rust version of 1.95. When the commands are executed within the checkout, rustup selects the pinned toolchain. The package selection, the binary selection, and the feature selection in the build command determine the command-line application that is produced.

The `--no-default-features` option omits the optional `dynamic` feature of the CLI, including the feature-gated FSI registration and the dynamic-results support. The static geometry and the provider composition used in this walkthrough remain available. Where the optional dynamic functionality is required, the application is built without that flag.

## Generate and inspect a model

The bundled examples are first copied into a local model directory. The selected document can then be inspected through `model show` and evaluated for surface export through `model export`.

On Linux or macOS:

```sh
./target/release/implexity model examples --copy models
./target/release/implexity model show models/03_lattice_block.json
./target/release/implexity model export models/03_lattice_block.json --node lattice_only --spacing 0.35 -o lattice.stl
```

In Windows PowerShell:

```powershell
.\target\release\implexity.exe model examples --copy models
.\target\release\implexity.exe model show models/03_lattice_block.json
.\target\release\implexity.exe model export models/03_lattice_block.json --node lattice_only --spacing 0.35 -o lattice.stl
```

The copy command writes the three bundled example documents into `models`, which provides an explicit location from which the subsequent commands obtain their input. A fresh destination can be selected where that directory already contains separately maintained models.

The lattice-block example combines a solid skin with a gyroid sheet core. The selection of `lattice_only` restricts the exported geometry to the confined infill. The coordinates and the sampling interval are expressed in millimetres; the selected sampling interval therefore determines the resolution at which the implicit surface is reconstructed. Its suitability depends on the dimensions of the geometric features and on the requirements of the subsequent analysis.

## Open the browser viewer

The local browser service is started from the repository root:

```sh
./target/release/implexity serve
```

On Windows, the corresponding command is `.\target\release\implexity.exe serve`. The interface is available at [http://127.0.0.1:8765](http://127.0.0.1:8765), and the service is terminated through Ctrl+C. The default host is the loopback interface. Where the default port is already occupied by another local process, an alternative is specified through `--port`.

The command `serve --workspace ./workspace` assigns the authored models and the associated setup to a dedicated writable directory. The [GUI walkthrough](gui.md) describes the geometric definition, the assignment of the physics and the ageing models, and the controls for the optimization and the result inspection.

Without a supplied design, the service initializes the **synthetic preview backend**. The models authored within the workbench use the implicit geometry document. The geometry JSON files used by the command-line workflow are distinct from the design `.npz` expected by `--design`; the backend options and their corresponding inputs are documented through `serve --help`.

## Explore optimization and interfaces

The available diagnostics, model operations, and study commands can be inspected through the following command-line entry points:

```sh
./target/release/implexity doctor
./target/release/implexity model --help
./target/release/implexity study --help
```

The study commands operate the MCP bridge and accept study plans that define the corresponding sequence of operations. Their execution requires the relevant study inputs and configuration; the publication-specific optimization campaigns are not reconstructed automatically from the presence of the manuscript or the bundled geometry examples.

The separate bridge executable is built through:

```sh
cargo build --release --locked -p implexity-cli --bin implexity-mcp --no-default-features
```

Some documentation and publication-scope paths referenced by the CLI help are absent from the inspected source revision. For those paths, the shipped command help and the source implementation provide the available description of the command interface.

## Recorded verification scope

The recorded installation check built the CLI on Windows with the already-installed Rust 1.99.1 stable toolchain, using `cargo +stable` with the package, binary, release, and feature selection shown above. The pinned toolchain and the optional dynamic configuration were not separately rebuilt for that check. The additional MCP bridge executable and a complete study execution were also outside its scope.

The lattice export produced a watertight STL containing 31,684 triangles at a sampling interval of 0.35 mm. This result establishes the operation of the model-authoring and surface-export path for the selected example. The appropriate sampling resolution for another geometry remains dependent on its feature dimensions and on the intended downstream evaluation.

The interpretation of a numerical result requires the selected model, the input data, the units, the constraints, the convergence behavior, and the extracted geometry to be considered together. The [method](method.md), the [research notes](research.md), and the repository [engineering disclaimer](../DISCLAIMER.md) provide the corresponding methodological and application context.
