# The differentiable design problem

Implexity considers the implicit geometry, the coupled physical response, and the evolving material state within a common gradient-based design formulation. The underlying relation follows from the dependence of the operating fields on the geometry and from the subsequent dependence of the material evolution on those operating fields. The geometric update can therefore account for the response reached under the prescribed service conditions, including the influence of the corresponding ageing history. The scaffold and divertor studies apply this principle through distinct geometric, physical, and numerical formulations.

## Multiphysics composition

The registered physics modules provide the state variables, the evaluated responses, the coupling ports, and the derivative contracts through which their contributions are connected to the shared design coordinates. Any number of compatible physical models can consequently contribute to the same optimization problem, with their responses assigned to the objectives or to the associated constraints. The algebraic and residual runtimes assemble the corresponding evaluation and sensitivity paths, while the selected constitutive and material-history models determine the dependence of the physical response on the evolving local material state.

The simultaneous consideration of these models means that their responses and their interdependencies contribute to a common design update. The order of the numerical execution is determined by the dependencies between the selected models and by the corresponding solver contracts. Independent evaluations may be arranged according to that execution structure, whereas a coupled state or history evaluation retains the ordering required by its formulation.

The relevant implementation is provided by the [physics orchestration](../crates/implexity-core/src/orchestration), the [coupled residual runtime](../crates/implexity-runtime/src/residual_runtime.rs), and the [material-history models](../crates/implexity-physics-solid/src/history).

## Geometry and the design variables

In the scaffold formulation, the geometry is represented by coarse control fields that define the integrable phase potentials, the bounded half-thickness, and the macroscopic topology mask. These fields are interpolated to the geometric or analysis discretization, through which the local modification of the lattice remains connected to a continuous set of design coordinates. The two-dimensional periodic function is defined as

$$f(x)=\cos\Phi_1(x)+\cos\Phi_2(x),\qquad \Phi_i(x)=k_0x_i+\delta\phi_i(x).$$

The gradients of the phase potentials jointly determine the local orientation and the local spacing. A regularized first-order distance surrogate, based on $|f|/|\nabla f|$, is used to distribute the material around the level set. The smooth occupancy and the separately penalized stiffness density establish the relation between the implicit geometry and the analysis grid. The admissibility of the phase map is encouraged through soft penalties, so that the resulting regularity remains dependent on the penalized optimization rather than on a hard admissibility constraint.

The three-dimensional divertor formulation extends this description through spatial fields governing the phase-map stretch, the phase-map warp, the TPMS basis blending, the sheet/network shape, the topology mask, the level-set corrections, the wall thickness, and the relative allocation of tungsten and CuCrZr. The control grid defines the geometric degrees of freedom, the geometry sampling grid resolves the resulting implicit fields, and the physical analysis grid supplies the discretization on which the selected physical problem is evaluated. These discretizations therefore have distinct functions within the overall calculation.

The geometric representation is implemented in the [lattice module](../crates/implexity-geometry/src/lattice), the [TPMS families](../crates/implexity-geometry/src/tpms.rs), and the [model documents](../crates/implexity-geometry/src/document).

## Physics and material history

Let $x$ denote the design, $G(x)$ the corresponding geometry, $z_n$ the physical state, and $h_n$ the material history at the indexed state. The general dependency can then be expressed as

$$x\longrightarrow G(x)\longrightarrow (z_n,h_n)\longrightarrow J.$$

The physical fields determine the local exposure and the subsequent state evolution, through which the constitutive properties entering the physical response are modified. Where these dependencies are included in the selected model, the differentiation returns their contribution to the geometric design variables. The influence of ageing on the design gradient is consequently determined by the specified evolution laws and by the dependencies retained in their evaluation.

In the scaffold study, the considered mechanisms include the thermoelastic loading, the cyclic deterioration, the geometry-dependent oxidative transport, the prescribed photolytic exposure, and the surrogate descriptions of tissue deposition and resorption. The geometric dependence differs between these mechanisms. In particular, the prescribed ultraviolet exposure remains a prescribed field, whereas the oxidative transport depends on the current geometry through the associated transport problem.

For the primary ageing comparison in the divertor study, a non-aged full-load solution supplies the reference temperature and the reference total strain. These reference fields remain fixed during the subsequent ageing increment, after which the aged full-load response is evaluated. The geometric sensitivity includes the reference solution, the history map, and the aged solution. The ageing dependence is therefore differentiated through the complete reference–history–response sequence, while the reference fields themselves are held constant within the history increment.

## Sensitivities and updates

For the scaffold equilibrium $K(p)u=b(p)$, the derivative of the response is obtained through an adjoint formulation, which avoids a separate state solution for each design variable:

$$K^T\lambda=\frac{\partial C}{\partial u},\qquad
\frac{dC}{dp}=\frac{\partial C}{\partial p}+\lambda^T\left(\frac{\partial b}{\partial p}-\frac{\partial K}{\partial p}u\right).$$

Each wrapped implicit linear equilibrium requires its corresponding adjoint solution. The derivative of a design-dependent load contributes through the load term and must consequently be retained alongside the derivative of the stiffness. For a coupled nonlinear history, the overall computational effort follows from the number and the structure of the participating state, history, and adjoint evaluations.

The scaffold implementation combines Adam with a volume-tangent gradient projection for the thickness and mask controls, a staged sharpening of the interface, and a bisected scalar restoration of the prescribed volume. The scalar volume restoration is performed outside the differentiated path. The primary divertor comparison uses box-normalized projected steepest descent with backtracking, an Armijo decrease test, and soft augmented-Lagrangian penalties for the bounded responses. The optimization procedure is thus specific to the numerical formulation used in each study.

For the time-indexed residual formulation, the divertor paper employs a reverse discrete adjoint through which the sensitivities are propagated through the coupled history. The corresponding implementation can be examined through the [automatic differentiation](../crates/implexity-ad), the [optimization](../crates/implexity-optim), and the [thermofluid history](../crates/implexity-physics-thermofluid/src/unified_history) modules.

## From a smooth field to an evaluated geometry

The smooth occupancy provides the continuous material description required by the differentiated formulation. The subsequent extraction of a solid geometry changes that description and can therefore change the evaluated response. The scaffold study examines this dependence through comparisons between the relaxed objectives and the volume-matched binary evaluations, alongside variations of the interface penalties, the mesh resolution, and the temporal discretization. The divertor study reconstructs the surfaces from sampled fields and characterizes the dependence of the resulting topology metrics on the extraction threshold.

The interpretation of a displayed surface consequently requires the geometric sampling, the physical discretization, and the extraction procedure to be considered together. The scope of the numerical comparisons and their relation to the underlying material and service assumptions are described in the [research notes](research.md).

The formulation is documented in the [scaffold paper](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7350458), Sections 3–4 and Appendices A–D, and in the [divertor paper](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7596080), Section 3 and Appendices A–B.
