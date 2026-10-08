// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
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
