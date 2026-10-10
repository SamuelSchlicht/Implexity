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

"use strict";


function hex(buffer) {
  return Array.from(new Uint8Array(buffer), value => value.toString(16).padStart(2, "0")).join("");
}

async function sha256(buffer) {
  if (!self.crypto || !self.crypto.subtle) {
    throw new Error("Web Crypto SHA-256 is unavailable; the field tile cannot be verified safely");
  }
  return hex(await self.crypto.subtle.digest("SHA-256", buffer));
}

async function gunzip(buffer) {
  if (typeof DecompressionStream !== "function") {
    throw new Error("gzip tile decoding is unavailable in this WebView; retry the tile with encoding=raw");
  }
  const stream = new Blob([buffer]).stream().pipeThrough(new DecompressionStream("gzip"));
  return await new Response(stream).arrayBuffer();
}

function dtypeReader(buffer, dtype) {
  const text = String(dtype || "<f4");
  const little = !text.startsWith(">");
  const code = text.replace(/[<>=|]/g, "");
  const view = new DataView(buffer);
  let bytes = 4, read;
  if (code === "f8") { bytes = 8; read = offset => view.getFloat64(offset, little); }
  else if (code === "f4") { bytes = 4; read = offset => view.getFloat32(offset, little); }
  else if (code === "i8") { bytes = 8; read = offset => Number(view.getBigInt64(offset, little)); }
  else if (code === "u8") { bytes = 8; read = offset => Number(view.getBigUint64(offset, little)); }
  else if (code === "i4") { bytes = 4; read = offset => view.getInt32(offset, little); }
  else if (code === "u4") { bytes = 4; read = offset => view.getUint32(offset, little); }
  else if (code === "i2") { bytes = 2; read = offset => view.getInt16(offset, little); }
  else if (code === "u2") { bytes = 2; read = offset => view.getUint16(offset, little); }
  else if (code === "i1") { bytes = 1; read = offset => view.getInt8(offset); }
  else if (code === "u1" || code === "b1") { bytes = 1; read = offset => view.getUint8(offset); }
  else throw new Error(`unsupported field tile dtype ${text}`);
  return { length: Math.floor(buffer.byteLength / bytes), read: index => read(index * bytes) };
}

function scalarise(buffer, header, componentMode) {
  const shape = (header.shape || header.spatial_shape || []).map(Number);
  if (shape.length < 3) throw new Error("field tile shape is missing");
  const spatialCount = shape.slice(0, 3).reduce((a, b) => a * b, 1);
  const components = Math.max(1, shape.slice(3).reduce((a, b) => a * b, 1));
  const source = dtypeReader(buffer, header.dtype);
  if (source.length !== spatialCount * components) {
    throw new Error(`tile contains ${source.length} values, expected exactly ${spatialCount * components}`);
  }
  const output = new Float32Array(spatialCount);
  const mode = componentMode == null ? (components > 1 ? "magnitude" : 0) : componentMode;
  if (mode === "magnitude") {
    for (let i = 0; i < spatialCount; i += 1) {
      let sum = 0;
      for (let c = 0; c < components; c += 1) {
        const value = source.read(i * components + c);
        sum += value * value;
      }
      output[i] = Math.sqrt(sum);
    }
  } else {
    const component = Math.max(0, Math.min(components - 1, Number(mode) || 0));
    for (let i = 0; i < spatialCount; i += 1) output[i] = source.read(i * components + component);
  }
  return output;
}

self.onmessage = async event => {
  const message = event.data || {};
  if (message.type !== "decode") return;
  const generation = message.generation;
  const requestId = message.requestId;
  try {
    let raw = message.buffer;
    if (!(raw instanceof ArrayBuffer)) throw new Error("tile worker received no ArrayBuffer");
    if (message.encoding === "gzip") raw = await gunzip(raw);
    const expected = String(message.header?.raw_sha256 || "");
    if (expected) {
      const actual = await sha256(raw);
      if (actual !== expected) throw new Error("field tile SHA-256 mismatch");
    }
    const values = scalarise(raw, message.header || {}, message.component);
    self.postMessage({
      type: "decoded", generation, requestId,
      header: message.header, values: values.buffer,
    }, [values.buffer]);
  } catch (error) {
    self.postMessage({
      type: "error", generation, requestId,
      error: String(error && error.message || error),
      retryRaw: /gzip/i.test(String(error && error.message || error)),
    });
  }
};
