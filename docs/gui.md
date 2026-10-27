# Using the graphical workbench

The graphical workbench provides a common interface for the definition of the implicit geometry, the assignment of the physical conditions, and the configuration of the optimization problem. The command-line application is first built according to the [installation guide](getting-started.md), after which the local service is started with a dedicated workspace:

```sh
./target/release/implexity serve --workspace ./workspace
```

For Windows PowerShell, the corresponding command is:

```powershell
.\target\release\implexity.exe serve --workspace .\workspace
```

The interface is available through [the local workbench](http://127.0.0.1:8765). The specified workspace directory contains the model and the authored setup, through which the geometric definition and the associated configuration are retained together.

## Geometry

The geometric description can be initialized through **New geometry…**, through the import of a mesh, or through **explore the worked example** in the empty viewport. The features are selected in **Model tree**, from which their parameters can be inspected and modified. A parameter is exposed to the optimization through **Make free**, while its associated bounds define the admissible range within which it may be varied.

The viewport provides the selection, the movement, the resizing, the surface dragging, and the field-editing controls required for the geometric definition. **Hand off to topology editing** provides the transition from the parametric model to the editable topology fields. The distinction between the geometric parameters and the spatial fields determines which aspects of the design are available for direct editing or for subsequent optimization.

![The Geometry workspace with the model tree, the parameter controls, and the implicit geometry.](assets/gui-geometry.png)

## Conditions and physics

The geometric regions, the applied loads, and the boundary conditions are defined through **Conditions** and **CAE setup**. The assignment of the physical conditions to the corresponding regions establishes the application-specific problem that is to be evaluated on the current geometry.

Within **Physics**, the required components are activated through **Manage physics add-ins**, after which the corresponding provider setup is opened. The activation makes the component available to the problem definition. The material descriptions and the coupling settings are then supplied through the setup, according to the configuration supported by that provider.

For the consideration of ageing, the supported material-history laws and their associated data are specified alongside the physical model. The selected laws determine how the material state evolves under the prescribed or computed exposure, while the coupling configuration determines how that evolving state contributes to the physical response. The material-history definition therefore forms part of the coupled problem and must be consistent with the selected physical components.

![The Physics workspace with the coupled-history and the environmental-ageing add-ins selected.](assets/gui-physics.png)

## Objectives and optimization

The responses required for the design problem are added through **Objectives** and assigned to their respective objective or bound roles. This assignment determines which responses are to be improved and which responses are evaluated against prescribed limits. The resulting optimization problem consequently depends on the selected physical models, the available responses, and the geometric degrees of freedom.

Within **Optimise**, **Check readiness** is used to review the configured problem before execution. The design bounds and the solver settings are considered alongside this readiness assessment, since they define the permitted geometric variation and the numerical conditions under which the physical problem is evaluated.

The execution is controlled through **Pause**, **Resume**, and **Stop**. Where a modification of the current design is required, **Manual intervention** and **Resume from modified design** provide the corresponding continuation controls. **Edit all settings…** opens the combined configuration of the physics, the objectives, the material history, the design, and the run settings, through which their consistency can be reviewed within the same interface.

## Results and output

The **Results** workspace provides access to the recorded designs, the physical fields, the material histories, and the design sensitivities. Their joint inspection allows the resulting geometry to be considered in relation to the physical response and to the corresponding material evolution. A recorded state can be returned to the editing workflow through **Use as working design**.

The **Output** menu provides the export of the meshes, the rendered images, and the geometric sections. The interface detail is adjusted through **Essential**, **Standard**, and **Advanced**, while **Model**, **Inspector**, and **Focus** control the arrangement of the panels.

The screenshots document the geometric authoring and the physical setup of the bundled example. They show the interface configuration without depicting the result of a completed optimization campaign.
