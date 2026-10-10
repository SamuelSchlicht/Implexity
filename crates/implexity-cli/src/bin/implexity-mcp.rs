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



#![forbid(unsafe_code)]

use std::process::ExitCode;

fn preserve_on_sigint() -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::io::Result<()>>();
    std::thread::Builder::new().name("implexity-mcp-sigint".into()).spawn(move || {
        runtime.block_on(async move {
            match tokio::signal::ctrl_c().await {
                Ok(()) => {}
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            }
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    return;
                }
            }
        });
    })?;

    std::thread::sleep(std::time::Duration::from_millis(20));
    match ready_rx.try_recv() {
        Ok(Err(e)) => Err(e),
        _ => Ok(()),
    }
}

fn main() -> ExitCode {
    if std::env::var("IMPLEXITY_MCP_PRESERVE_ON_SIGINT").as_deref() == Ok("1")
        && let Err(e) = preserve_on_sigint()
    {
        eprintln!("Implexity MCP configuration error: cannot preserve on SIGINT: {e}");
        return ExitCode::from(2);
    }
    let mut client = match implexity_mcp::client_from_environment() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Implexity MCP configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    match implexity_mcp::serve(&mut client, stdin, stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}
