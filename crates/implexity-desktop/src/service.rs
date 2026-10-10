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



use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::host::Log;



pub(crate) fn free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    Ok(listener.local_addr()?.port())
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_owned() }
}



pub(crate) fn service_command(root: &Path, port: u16) -> Result<(PathBuf, Vec<String>), String> {
    let name = exe("implexity");
    let parent = root.parent().unwrap_or(root);
    let candidates =
        [root.join(&name), parent.join("Service").join(&name), parent.join("ImplexityService").join(&name)];
    let Some(program) = candidates.into_iter().find(|p| p.is_file()) else {
        return Err(format!("{name} was not found beside the installed workbench"));
    };
    let args = ["serve", "--host", "127.0.0.1", "--port", &port.to_string(), "--backend", "auto", "--warm"]
        .map(str::to_owned)
        .to_vec();
    Ok((program, args))
}

pub(crate) fn service_env(root: &Path, user: &Path) -> Vec<(String, String)> {
    let mut defaults = vec![
        ("IMPLEXITY_CASE_DIR".to_owned(), user.join("state").display().to_string()),
        ("IMPLEXITY_HOME".to_owned(), root.display().to_string()),
    ];
    defaults.retain(|(k, _)| std::env::var_os(k).is_none());
    defaults.push(("IMPLEXITY_EXIT_ON_STDIN_EOF".to_owned(), "1".to_owned()));
    defaults
}

#[derive(Debug)]
pub(crate) struct Service {
    child: Child,
}

impl Service {


    pub(crate) fn start(root: &Path, user: &Path, port: u16, log: &Log) -> Result<Self, String> {
        let (program, args) = service_command(root, port)?;
        log.info(&format!(
            "starting service: {:?}",
            std::iter::once(program.display().to_string()).chain(args.iter().cloned()).collect::<Vec<_>>()
        ));
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(user.join("logs").join("service.log"))
            .map_err(|e| e.to_string())?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let mut command = Command::new(&program);
        command.args(&args).current_dir(root).stdin(Stdio::piped()).stdout(out).stderr(err);
        for (k, v) in service_env(root, user) {
            command.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let child = command.spawn().map_err(|e| e.to_string())?;
        Ok(Self { child })
    }



    pub(crate) fn wait_ready(&mut self, url: &str, timeout: Duration) -> Result<Value, String> {
        let deadline = Instant::now() + timeout;
        let config = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_millis(1500)))
            .http_status_as_error(true)
            .build();
        let agent = ureq::Agent::new_with_config(config);
        let mut last = "service did not answer".to_owned();
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                let code = status.code().map_or_else(|| "None".to_owned(), |c| c.to_string());
                return Err(format!("ImplexityService exited with code {code}"));
            }
            let reply = agent
                .get(format!("{url}/v1/implicit/catalogue"))
                .header("Cache-Control", "no-cache")
                .call()
                .map_err(|e| e.to_string())
                .and_then(|mut r| r.body_mut().read_to_string().map_err(|e| e.to_string()))
                .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()));
            match reply {
                Ok(v) => return Ok(v),
                Err(e) => {
                    last = e;
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
        Err(format!("ImplexityService did not become ready: {last}"))
    }

    pub(crate) fn stop(mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

