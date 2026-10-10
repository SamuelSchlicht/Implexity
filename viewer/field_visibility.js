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

(() => {
  "use strict";

  const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
  const add = (a, b) => [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
  const scale = (a, s) => [a[0] * s, a[1] * s, a[2] * s];
  const sub = (a, b) => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
  const norm = a => Math.hypot(a[0], a[1], a[2]);

  function registration(value) {
    if (!value || !Array.isArray(value.origin) || !Array.isArray(value.basis)
        || !Array.isArray(value.shape) || value.shape.length !== 3) {
      throw new TypeError("exact field registration is required");
    }
    const basis = value.basis.map(axis => axis.slice(0, 3).map(Number));
    if (basis.length !== 3 || basis.some(axis => axis.length !== 3 || axis.some(v => !Number.isFinite(v)))) {
      throw new TypeError("registration basis must contain three finite vectors");
    }
    const origin = value.origin.slice(0, 3).map(Number);
    const shape = value.shape.slice(0, 3).map(Number);
    if (origin.some(v => !Number.isFinite(v)) || shape.some(v => !Number.isFinite(v) || v < 1)) {
      throw new TypeError("registration origin and shape must be finite");
    }
    return { ...value, origin, basis, shape };
  }

  function world(registrationValue, indexBoundary) {
    const reg = registration(registrationValue);
    let point = reg.origin.slice();
    for (let axis = 0; axis < 3; axis += 1) point = add(point, scale(reg.basis[axis], indexBoundary[axis]));
    return point;
  }

  function tileBounds(registrationValue, offset, shape) {
    const reg = registration(registrationValue);
    const extent = shape.map(value => reg.centering === "node" ? Math.max(0, value - 1) : value);
    const corners = [];
    for (let bx = 0; bx < 2; bx += 1) {
      for (let by = 0; by < 2; by += 1) {
        for (let bz = 0; bz < 2; bz += 1) {
          corners.push(world(reg, [
            offset[0] + (bx ? extent[0] : 0),
            offset[1] + (by ? extent[1] : 0),
            offset[2] + (bz ? extent[2] : 0),
          ]));
        }
      }
    }
    const centre = corners.reduce((sum, value) => add(sum, value), [0, 0, 0]).map(v => v / corners.length);
    const radius = Math.max(...corners.map(value => norm(sub(value, centre))));
    return { corners, centre, radius };
  }

  function cameraCoordinates(point, frustum) {
    const relative = sub(point, frustum.eye);
    return {
      side: dot(relative, frustum.right || [1, 0, 0]),
      rise: dot(relative, frustum.up || [0, 1, 0]),
      depth: dot(relative, frustum.fwd),
    };
  }

  function visibleSphere(bounds, frustum) {
    if (!frustum || !Array.isArray(frustum.eye) || !Array.isArray(frustum.fwd)) return true;
    const corners = Array.isArray(bounds?.corners) && bounds.corners.length
      ? bounds.corners : [bounds.centre];
    const points = corners.map(point => cameraCoordinates(point, frustum));
    const vertical = Math.tan((Number(frustum.fov) || 32) * Math.PI / 360);
    const horizontal = vertical * Math.max(Number(frustum.aspect) || 1, 1e-6);
    const near = Number.isFinite(Number(frustum.near)) ? Number(frustum.near) : 0;
    const far = Number.isFinite(Number(frustum.far)) ? Number(frustum.far) : Number.POSITIVE_INFINITY;
    const allOutside = predicate => points.every(predicate);


    if (allOutside(p => p.depth <= near)) return false;
    if (Number.isFinite(far) && allOutside(p => p.depth >= far)) return false;
    if (allOutside(p => p.depth * horizontal + p.side < 0)) return false;
    if (allOutside(p => p.depth * horizontal - p.side < 0)) return false;
    if (allOutside(p => p.depth * vertical + p.rise < 0)) return false;
    if (allOutside(p => p.depth * vertical - p.rise < 0)) return false;
    return true;
  }

  function projectedRadius(bounds, frustum) {
    if (!frustum || !Array.isArray(frustum.eye) || !Array.isArray(frustum.fwd)) return 0;
    const { depth } = cameraCoordinates(bounds.centre, frustum);
    if (!(depth > 0)) return 0;
    const tangent = Math.max(Math.tan((Number(frustum.fov) || 32) * Math.PI / 360), 1e-9);
    const height = Math.max(Number(frustum.height) || 1, 1);
    return bounds.radius * height / (2 * depth * tangent);
  }

  function priorityComponents(bounds, frustum, focus) {
    const visible = visibleSphere(bounds, frustum);
    const eye = frustum?.eye || [0, 0, 0];
    return {
      visible,
      focusDistance: focus ? norm(sub(bounds.centre, focus)) : 0,
      screenRadius: projectedRadius(bounds, frustum),
      cameraDistance: norm(sub(bounds.centre, eye)),
    };
  }

  function comparePriority(a, b) {
    if (a.visible !== b.visible) return a.visible ? -1 : 1;
    if (a.focusDistance !== b.focusDistance) return a.focusDistance - b.focusDistance;
    if (a.screenRadius !== b.screenRadius) return b.screenRadius - a.screenRadius;
    if (a.cameraDistance !== b.cameraDistance) return a.cameraDistance - b.cameraDistance;
    return 0;
  }

  function priority(bounds, frustum, focus) {
    const value = priorityComponents(bounds, frustum, focus);


    return (value.visible ? 0 : 1e15)
      + value.focusDistance * 1e6
      + value.cameraDistance
      - Math.min(value.screenRadius, 1e6) * 1e-3;
  }

  function descriptors(level, tileShape, frustum, focus) {
    const counts = level.tile_counts || [1, 1, 1];
    const spatial = level.spatial_shape.map(Number);
    const items = [];
    for (let x = 0; x < counts[0]; x += 1) {
      for (let y = 0; y < counts[1]; y += 1) {
        for (let z = 0; z < counts[2]; z += 1) {
          const index = [x, y, z];
          const offset = index.map((v, axis) => v * tileShape[axis]);
          const shape = offset.map((v, axis) => Math.min(tileShape[axis], spatial[axis] - v));
          const bounds = tileBounds(level.registration, offset, shape);
          const order = priorityComponents(bounds, frustum, focus);
          items.push({ index, offset, shape, bounds, ...order, _order: order });
        }
      }
    }
    items.sort((a, b) => comparePriority(a._order, b._order)
      || a.index.join(",").localeCompare(b.index.join(",")));
    items.forEach((item, ordinal) => {
      item.priority = ordinal;
      delete item._order;
    });
    return items;
  }

  globalThis.ImplexityFieldVisibility = Object.freeze({
    registration, world, tileBounds, visibleSphere, projectedRadius,
    priorityComponents, comparePriority, priority, descriptors,
  });
})();
