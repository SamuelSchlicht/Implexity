# Using the graphical workbench

Build the command-line application using the [installation guide](getting-started.md), then start a local workspace:

```sh
./target/release/implexity serve --workspace ./workspace
```

On Windows:

```powershell
.\target\release\implexity.exe serve --workspace .\workspace
```

Open [the workbench](http://127.0.0.1:8765). The workspace directory stores the model and authored setup.

## Geometry

Choose **New geometry…**, import a mesh, or use **explore the worked example** on the empty viewport. Select features in **Model tree** and edit their parameters. **Make free** exposes a parameter to optimization; its bounds define the permitted range.

The viewport provides selection, movement, resizing, surface dragging, and field-editing tools. Use **Hand off to topology editing** when moving from a parametric model to editable topology fields.

![Geometry workspace with the bundled model.](assets/gui-geometry.png)

## Conditions and physics

Use **Conditions** and **CAE setup** to define regions, loads, and boundary conditions. In **Physics**, expand **Manage physics add-ins**, activate the required components, and open the provider setup.

For ageing, configure the supported material-history laws and their data alongside the physical model. Add-in activation makes a component available; its coupling and material settings are supplied in the setup.

![Physics workspace with coupled-history and environmental-ageing add-ins selected.](assets/gui-physics.png)

## Objectives and optimization

In **Objectives**, add the required responses and choose their objective or bound roles. In **Optimise**, review **Check readiness**, design bounds, and solver settings before starting.

**Pause**, **Resume**, and **Stop** control a run. **Manual intervention** and **Resume from modified design** support continued work from an edited state. **Edit all settings…** opens the combined physics, objectives, history, design, and run configuration.

## Results and output

Open **Results** to inspect recorded designs, physical fields, material histories, and sensitivities. **Use as working design** brings a recorded design back into the editing workflow.

The **Output** menu provides mesh export, rendered images, and sections. **Essential**, **Standard**, and **Advanced** adjust interface detail; **Model**, **Inspector**, and **Focus** control the panel arrangement.

The screenshots show authoring views of the bundled example. No optimization result is depicted.
