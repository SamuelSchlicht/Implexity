# The differentiable design problem

Implexity combines implicit geometry, coupled physics, and material ageing in a shared gradient-based optimization problem. The supplied manuscripts use different numerical formulations within this approach.

## Multiphysics composition

Registered physics modules contribute states, responses, coupling ports, and derivative contracts to shared design coordinates. An extensible set of physical responses can serve as objectives and constraints in the same optimization. The algebraic and residual runtimes assemble their evaluation and sensitivity paths; selected constitutive and history models supply the ageing-dependent material response.

Simultaneous optimization refers to these responses informing a common design update. Numerical execution follows the dependencies and solver contracts of the selected models.

Implementation: [physics orchestration](../crates/implexity-core/src/orchestration), [coupled residual runtime](../crates/implexity-runtime/src/residual_runtime.rs), and [material-history models](../crates/implexity-physics-solid/src/history).

## Geometry and the design variables

In the scaffold paper, coarse control fields define integrable phase potentials, bounded half-thickness, and a macroscopic topology mask. The two-dimensional periodic function is

$$f(x)=\cos\Phi_1(x)+\cos\Phi_2(x),\qquad \Phi_i(x)=k_0x_i+\delta\phi_i(x).$$

The phase gradients jointly control orientation and spacing. A regularized first-order distance surrogate, based on $|f|/|\nabla f|$, places material around the level set. Smooth occupancy and a separately penalized stiffness density connect the geometry to the analysis grid. Phase-map admissibility is encouraged by soft penalties; it is not guaranteed by hard constraints.

The three-dimensional divertor formulation adds spatial fields governing phase-map stretch and warp, TPMS basis blending, sheet/network shape, topology masking, level-set corrections, wall thickness, and relative W/CuCrZr allocation. The control grid, geometry sampling grid, and physical analysis grid have distinct roles.

The source implementation can be explored in [the lattice module](../crates/implexity-geometry/src/lattice), [TPMS families](../crates/implexity-geometry/src/tpms.rs), and [model documents](../crates/implexity-geometry/src/document).

## Physics and material history

Write the design as $x$, geometry as $G(x)$, physical state as $z_n$, and material history as $h_n$. The general dependency is

$$x\longrightarrow G(x)\longrightarrow (z_n,h_n)\longrightarrow J.$$

The physical fields determine exposure and state evolution. The resulting material properties feed into the physical response. Differentiation includes that dependence wherever it is present in the selected model.

In the scaffold study, the selected couplings include thermoelastic loads, cyclic deterioration, geometry-dependent oxidative transport, a prescribed photolytic exposure, and surrogate tissue deposition/resorption. These mechanisms have different dependencies: a prescribed UV profile does not acquire geometry dependence merely because other states do.

For the divertor primary ageing comparison, a non-aged full-load solution supplies reference temperature and total strain, which remain fixed during the ageing increment. An aged full-load response follows. The geometry sensitivity includes the reference solve, the history map, and the aged solve. This approximation should not be described as continuous recalculation of all reference fields throughout the ageing interval.

## Sensitivities and updates

For the scaffold linear equilibrium $K(p)u=b(p)$, an adjoint supplies the derivative without one state solve per design variable:

$$K^T\lambda=\frac{\partial C}{\partial u},\qquad
\frac{dC}{dp}=\frac{\partial C}{\partial p}+\lambda^T\left(\frac{\partial b}{\partial p}-\frac{\partial K}{\partial p}u\right).$$

Each wrapped implicit linear equilibrium requires an additional adjoint solve. Design-dependent loads must also be differentiated. This count does not imply that a coupled nonlinear history requires only one solve in total.

The scaffold implementation uses Adam, a volume-tangent gradient projection for the thickness and mask controls, staged interface sharpening, and a bisected scalar volume restoration **outside** the differentiated path. The divertor primary comparison uses box-normalized projected steepest descent, backtracking, an Armijo test, and soft augmented-Lagrangian response penalties. They should not be presented as one universal optimizer recipe.

For a time-indexed residual, the divertor paper uses a reverse discrete adjoint to return sensitivities through the coupled history. See [automatic differentiation](../crates/implexity-ad), [optimization](../crates/implexity-optim), and [thermofluid history](../crates/implexity-physics-thermofluid/src/unified_history) for implementation entry points.

## From a smooth field to an evaluated geometry

Smooth occupancy helps differentiability, but the extracted solid can perform differently. The scaffold paper explicitly compares relaxed and volume-matched binary responses and studies interface penalties, mesh resolution, and temporal discretization. The divertor paper reconstructs surfaces from sampled fields and examines threshold-dependent topology metrics.

A displayed surface, an analysis discretization, and a validated manufactured component are different objects. The [research notes](research.md) describe the scope of the comparisons and their limitations.

Sources: [scaffold paper](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7350458), Sections 3–4 and Appendices A–D; [divertor paper](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=7596080), Section 3 and Appendices A–B.
