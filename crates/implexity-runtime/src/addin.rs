// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// # Open-access statement and disclaimer
//
// ## Open-access statement
// Metaplexis Ingenieurgesellschaft GmbH believes that open access to advanced computer-aided engineering (CAE) tools is essential to advancing research. Computational methods are developing rapidly and can explore scales and levels of complexity beyond those previously practical for unaided human workflows. By publishing Implexity, Metaplexis seeks to enable transparent inspection, reproducibility, independent validation, and collaborative scientific and engineering progress.
//
// This statement describes Metaplexis’s motivation. It is not a representation or guarantee of the software’s capabilities, accuracy, fitness, safety, or regulatory status.
//
// ## Relationship to the license
// This notice is informational. It does not amend the Apache License, Version 2.0 (the “License”), or impose any additional condition, restriction, or field-of-use limitation. Permissions and conditions for first-party software are governed by the License. Nothing in this notice creates a warranty, guarantee, professional duty, support obligation, or acceptance of additional liability. Third-party material remains governed by its own notices and license terms.
//
// ## Engineering and validation status
// Implexity is engineering software. Bundled models, examples, simulations, optimization outputs, geometry, renderings, and other results may be reduced-order, approximate, screening, exploratory, non-converged, or unvalidated unless expressly stated otherwise. They are not, by themselves, certification, qualification, design approval, a safety case, or professional engineering advice.
//
// Users remain responsible for selecting and validating models and data; checking inputs, units, assumptions, boundary conditions, convergence, sensitivities, constraints, materials, geometry, and outputs; obtaining suitably qualified review; and complying with applicable laws, codes, standards, and safety requirements. For any use in which failure could cause death, personal injury, environmental harm, or substantial property damage, users should apply an independently verified engineering and safety process and appropriate safeguards. These statements describe validation limits; they do not prohibit any use permitted by the License.
//
// ## Warranty and liability
// Sections 7 and 8 of the License apply to the maximum extent permitted by applicable law. Nothing in this notice excludes or limits liability where exclusion or limitation is unlawful.
//
// Where German law applies, liability remains unaffected for intentional or grossly negligent conduct by Metaplexis, its statutory representatives, or vicarious agents; for culpable injury to life, body, or health; for mandatory liability under the German Product Liability Act (Produkthaftungsgesetz); and, where applicable, for claims within the scope of an expressly assumed guarantee or arising from fraudulent concealment of a defect.
//
// In the event of a slightly negligent breach of a material contractual obligation (an obligation whose performance is essential to proper performance of the contract and on whose performance the other party may regularly rely), liability, to the extent legally permissible, is limited to damage foreseeable and typical for the contract. The statutory allocation of the burden of proof remains unchanged.
//
// This savings clause does not create liability beyond liability imposed by applicable law or expressly agreed in writing.
// METAPLEXIS-DISCLAIMER-END





use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use implexity_ad::tape::{Tape, Var};
use implexity_core::orchestration::{AddInAdapter, OrchestrationPlan};
use implexity_core::{CaeError, CaeResult};
use implexity_optim::design::NamedArrays;
use implexity_solve::matrix::Jacobian;
use ndarray::ArrayD;
use serde_json::{Map, Value};

pub const ADDIN_OPERATIONS: &str = "implexity.cae.addin_operations";

pub type JsonMap = Map<String, Value>;

#[allow(unused_variables)]
pub trait AddInOperations: Send + Sync {
    fn has_operation(&self, operation: &str) -> bool {
        false
    }


    fn call_operation(
        &self,
        operation: &str,
        context: &JsonMap,
        plan: &OrchestrationPlan,
    ) -> CaeResult<Option<JsonMap>> {
        Err(CaeError::contract(format!("adapter has no {operation} operation")))
    }

    fn has_execute(&self) -> bool {
        false
    }


    fn execute(
        &self,
        operation: &str,
        context: &JsonMap,
        plan: &OrchestrationPlan,
    ) -> CaeResult<Option<JsonMap>> {
        Err(CaeError::contract("adapter has no execute operation"))
    }

    fn has_extend(&self) -> bool {
        false
    }


    fn extend(
        &self,
        result: &JsonMap,
        operation: &str,
        context: &JsonMap,
        plan: &OrchestrationPlan,
    ) -> CaeResult<Option<JsonMap>> {
        Err(CaeError::contract("adapter has no extend operation"))
    }

    fn provides(&self, hook: &str) -> bool {
        false
    }

    fn residual_contributions(&self, context: &JsonMap) -> Option<CaeResult<Vec<ResidualContribution>>> {
        None
    }

    fn response_contributions(&self, context: &JsonMap) -> Option<CaeResult<Vec<ResponseContribution>>> {
        None
    }

    fn field_contributions(&self, context: &JsonMap) -> Option<CaeResult<Vec<FieldContribution>>> {
        None
    }

    fn numerical_port_bindings(&self, context: &JsonMap) -> Option<CaeResult<Vec<NumericalPortBinding>>> {
        None
    }

    fn algebraic(&self) -> Option<&dyn AlgebraicAddIn> {
        None
    }

    fn coupling_validation(&self, problem: &Value, for_optimization: bool) -> Option<CaeResult<Value>> {
        None
    }

    fn normalise_problem(&self, problem: &Value) -> Option<CaeResult<Value>> {
        None
    }
}

pub struct AddInInterface {
    cast: fn(&dyn Any) -> Option<&dyn AddInOperations>,
}

impl std::fmt::Debug for AddInInterface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AddInInterface")
    }
}

fn cast_to<A: AddInOperations + 'static>(any: &dyn Any) -> Option<&dyn AddInOperations> {
    any.downcast_ref::<A>().map(|a| a as &dyn AddInOperations)
}

struct InterfaceOf<A>(std::marker::PhantomData<A>);

impl<A: AddInOperations + 'static> InterfaceOf<A> {
    const INTERFACE: AddInInterface = AddInInterface { cast: cast_to::<A> };
}

#[must_use]
pub fn addin_interface<A: AddInOperations + 'static>(name: &str) -> Option<&'static (dyn Any + Send + Sync)> {
    if name == ADDIN_OPERATIONS {
        let iface: &'static AddInInterface = &InterfaceOf::<A>::INTERFACE;
        Some(iface)
    } else {
        None
    }
}

#[must_use]
pub fn addin_operations(adapter: &dyn AddInAdapter) -> Option<&dyn AddInOperations> {
    let iface = adapter.interface(ADDIN_OPERATIONS)?.downcast_ref::<AddInInterface>()?;
    (iface.cast)(adapter.as_any())
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateBlock {
    pub name: String,
    pub size: usize,
    pub initial: Vec<f64>,
    pub residual_scale: Vec<f64>,
    pub residual_units: String,
}

impl StateBlock {
    #[must_use]
    pub fn new(name: &str, size: usize, initial: Vec<f64>) -> Self {
        Self { name: name.to_string(), size, initial, residual_scale: vec![1.0], residual_units: "1".into() }
    }
}

pub type States = BTreeMap<String, Vec<f64>>;


pub trait ResidualCallbacks: Send + Sync {

    fn residual(&self, states: &States, design: &NamedArrays, context: &JsonMap) -> CaeResult<Vec<f64>>;

    fn state_jacobians(
        &self,
        states: &States,
        design: &NamedArrays,
        context: &JsonMap,
    ) -> CaeResult<BTreeMap<String, Jacobian>>;

    fn design_jacobians(
        &self,
        states: &States,
        design: &NamedArrays,
        context: &JsonMap,
    ) -> CaeResult<BTreeMap<String, Jacobian>>;
}

#[derive(Clone)]
pub struct ResidualContribution {
    pub state: StateBlock,
    pub callbacks: Arc<dyn ResidualCallbacks>,
    pub named: bool,
    pub design_coordinates: Vec<String>,
    pub state_dependencies: Option<Vec<String>>,
}

impl std::fmt::Debug for ResidualContribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResidualContribution")
            .field("state", &self.state.name)
            .field("named", &self.named)
            .field("design_coordinates", &self.design_coordinates)
            .field("state_dependencies", &self.state_dependencies)
            .finish_non_exhaustive()
    }
}

pub trait ResponseCallbacks: Send + Sync {

    fn value(&self, states: &States, design: &NamedArrays, context: &JsonMap) -> CaeResult<f64>;

    fn state_gradients(&self, states: &States, design: &NamedArrays, context: &JsonMap) -> CaeResult<States>;

    fn design_gradients(
        &self,
        states: &States,
        design: &NamedArrays,
        context: &JsonMap,
    ) -> CaeResult<NamedArrays>;
}

#[derive(Clone)]
pub struct ResponseContribution {
    pub name: String,
    pub callbacks: Arc<dyn ResponseCallbacks>,
    pub named: bool,
    pub design_coordinates: Vec<String>,
    pub state_dependencies: Option<Vec<String>>,
}

impl std::fmt::Debug for ResponseContribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseContribution")
            .field("name", &self.name)
            .field("named", &self.named)
            .field("design_coordinates", &self.design_coordinates)
            .finish_non_exhaustive()
    }
}

pub trait FieldCallback: Send + Sync {

    fn value(&self, states: &States, design: &NamedArrays, context: &JsonMap) -> CaeResult<ArrayD<f64>>;
}

pub type RegistrationFn = Arc<dyn Fn(&States, &NamedArrays, &JsonMap) -> CaeResult<Value> + Send + Sync>;

#[derive(Clone)]
pub enum FieldRegistration {
    Fixed(Value),
    Computed(RegistrationFn),
}

impl std::fmt::Debug for FieldRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fixed(v) => f.debug_tuple("Fixed").field(v).finish(),
            Self::Computed(_) => f.write_str("Computed"),
        }
    }
}

#[derive(Clone)]
pub struct FieldContribution {
    pub name: String,
    pub value: Arc<dyn FieldCallback>,
    pub units: String,
    pub association: String,
    pub domain: String,
    pub components: Vec<String>,
    pub registration: Option<FieldRegistration>,
}

impl std::fmt::Debug for FieldContribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldContribution")
            .field("name", &self.name)
            .field("units", &self.units)
            .field("association", &self.association)
            .field("domain", &self.domain)
            .field("components", &self.components)
            .field("registration", &self.registration)
            .finish_non_exhaustive()
    }
}

pub trait PortAction: Send + Sync {

    fn forward_jvp(
        &self,
        states: &States,
        design: &NamedArrays,
        direction: &[f64],
        context: &JsonMap,
    ) -> CaeResult<Vec<f64>>;

    fn transpose_vjp(
        &self,
        states: &States,
        design: &NamedArrays,
        direction: &[f64],
        context: &JsonMap,
    ) -> CaeResult<Vec<f64>>;
}

#[derive(Clone)]
pub struct NumericalPortBinding {
    pub source_addin: String,
    pub target_addin: String,
    pub source_port_id: String,
    pub target_port_id: String,
    pub state_dependencies: Vec<String>,
    pub action: Arc<dyn PortAction>,
}

impl std::fmt::Debug for NumericalPortBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NumericalPortBinding")
            .field("source_addin", &self.source_addin)
            .field("target_addin", &self.target_addin)
            .field("source_port_id", &self.source_port_id)
            .field("target_port_id", &self.target_port_id)
            .field("state_dependencies", &self.state_dependencies)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AlgebraicValue {
    Tensor {
        var: Var,
        shape: Vec<usize>,
    },
    Json(Value),
}

#[derive(Debug, Clone, Default)]
pub struct AlgebraicOutputs {
    pub outputs: Vec<(String, AlgebraicValue)>,
    pub validity: Option<Vec<(String, Vec<f64>)>>,
}

#[allow(unused_variables)]
pub trait AlgebraicAddIn: Send + Sync {
    fn named(&self) -> bool {
        true
    }

    fn reports_validity(&self) -> bool {
        false
    }

    fn design_coordinates(&self) -> Option<Vec<String>> {
        None
    }

    fn nonnegative_validity_margins(&self) -> Vec<String> {
        Vec::new()
    }


    fn evaluate(
        &self,
        tape: &mut Tape,
        inputs: &BTreeMap<String, AlgebraicValue>,
        design: &BTreeMap<String, AlgebraicValue>,
        context: &JsonMap,
    ) -> CaeResult<AlgebraicOutputs>;

    fn response_uses_design(&self) -> bool {
        true
    }


    fn response_value(
        &self,
        tape: &mut Tape,
        response: &str,
        outputs: &BTreeMap<String, AlgebraicValue>,
        design: Option<&BTreeMap<String, AlgebraicValue>>,
        context: &JsonMap,
    ) -> CaeResult<AlgebraicValue>;
}


pub fn require_ops<'a>(adapter: &'a dyn AddInAdapter, message: &str) -> CaeResult<&'a dyn AddInOperations> {
    addin_operations(adapter).ok_or_else(|| CaeError::contract(message.to_string()))
}
