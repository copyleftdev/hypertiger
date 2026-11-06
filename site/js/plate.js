// The film plate. Everything on it is a pure function of the time uniform, so scrubbing backwards costs
// the same as playing forwards and no state can drift from the run it shows.
import {
  BufferAttribute,
  BufferGeometry,
  LineSegments,
  Mesh,
  PerspectiveCamera,
  PlaneGeometry,
  Points,
  Scene,
  ShaderMaterial,
  Vector3,
  WebGLRenderer,
} from "../vendor/three.module.min.js";

const DISTANCE = 1 / Math.tan((15 * Math.PI) / 180);
const SPACING = 0.0062;

const BUBBLE_VERTEX = /* glsl */ `
  attribute vec2 aOrigin;
  attribute float aBirth, aS, aKind, aSettle, aOutcome, aOrder, aSeed, aSize;
  uniform float uTime, uSize, uDim, uKeep, uFocusA, uFocusB, uLocal, uBranch;
  uniform vec2 uHalf;
  varying vec3 vColor;
  varying float vAlpha;

  const vec3 PAPER = vec3(0.957, 0.945, 0.902);
  const vec3 YELLOW = vec3(1.0, 0.784, 0.0);
  const vec3 RED = vec3(1.0, 0.239, 0.18);

  void hide() {
    gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
    gl_PointSize = 0.0;
    vAlpha = 0.0;
    vColor = PAPER;
  }

  void main() {
    float age = uTime - aBirth;
    if (age < 0.0) { hide(); return; }

    bool branch = aKind > 0.5;
    bool settled = uTime >= aSettle;
    bool landedWorld = aOutcome > 0.5;
    bool isLanded = aKind > 0.5 && aKind < 1.5;
    bool winner = !branch || (isLanded ? landedWorld : !landedWorld);
    float focus = (abs(aOrder - uFocusA) < 0.5 || abs(aOrder - uFocusB) < 0.5) ? 1.0 : 0.0;

    float grow = 1.0;
    float alpha = 1.0;
    vec3 color = PAPER;

    if (branch && !settled) {
      // In doubt: dashed, whatever the hue, so the state survives without colour.
      if (aKind < 2.5 && fract(aS / 0.05) > 0.55) { hide(); return; }
      color = YELLOW;
    } else if (branch && !winner) {
      // The world that did not happen collapses, in bubbles that burst unevenly.
      float gone = (uTime - aSettle - aSeed * 0.5) / 0.32;
      if (aBirth >= aSettle || gone >= 1.0) { hide(); return; }
      gone = max(gone, 0.0);
      grow = 1.0 + 2.6 * gone;
      alpha = 1.0 - gone;
      color = YELLOW;
    } else if (branch && !landedWorld) {
      color = RED;
    }

    float nucleate = smoothstep(0.0, 0.05, age) * (1.0 + 0.9 * exp(-age * 12.0));
    float fade = branch
      ? mix(1.0, 0.5, smoothstep(5.0, 30.0, age))
      : mix(mix(1.0, 0.56, smoothstep(0.4, 9.0, age)), 0.2, smoothstep(9.0, 36.0, age));
    float kept = focus * uKeep;
    fade = mix(fade, 1.0, kept);
    alpha *= fade * mix(1.0 - uDim * 0.93, 1.0, kept);

    vec3 world = vec3(aOrigin * uHalf + position.xy * uLocal, position.z * uLocal);
    vec4 view = modelViewMatrix * vec4(world, 1.0);
    gl_Position = projectionMatrix * view;
    gl_PointSize = clamp(uSize * aSize * (branch ? uBranch : 1.0) * grow * nucleate * (${DISTANCE.toFixed(4)} / -view.z), 0.0, 22.0);
    vColor = color;
    vAlpha = alpha;
  }
`;

const BUBBLE_FRAGMENT = /* glsl */ `
  varying vec3 vColor;
  varying float vAlpha;
  void main() {
    float r = length(gl_PointCoord - 0.5);
    float edge = smoothstep(0.5, 0.36, r);
    if (edge * vAlpha < 0.01) discard;
    gl_FragColor = vec4(vColor, edge * vAlpha);
  }
`;

const GROUND_VERTEX = /* glsl */ `
  varying vec2 vUv;
  void main() {
    vUv = uv;
    gl_Position = vec4(position.xy, 0.0, 1.0);
  }
`;

// Film, not a gradient: uneven development, emulsion scratches running with the transport, and grain.
const GROUND_FRAGMENT = /* glsl */ `
  varying vec2 vUv;
  uniform vec2 uHalf;
  uniform float uDim;
  float hash(vec2 p) {
    vec3 q = fract(vec3(p.xyx) * 0.1031);
    q += dot(q, q.yzx + 33.33);
    return fract((q.x + q.y) * q.z);
  }
  float noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1, 0)), f.x),
      mix(hash(i + vec2(0, 1)), hash(i + vec2(1, 1)), f.x), f.y);
  }
  void main() {
    vec2 p = vUv * uHalf * 2.0;
    float blotch = noise(p * 1.3) * 0.6 + noise(p * 3.1 + 7.0) * 0.4;
    vec3 base = vec3(0.039, 0.11, 0.541);
    base *= 0.84 + 0.3 * blotch;
    float column = floor(vUv.x * 900.0);
    float scratch = step(0.9965, hash(vec2(column, 3.0))) * smoothstep(0.35, 0.8, noise(vec2(column, vUv.y * 6.0)));
    base += scratch * vec3(0.03, 0.035, 0.08);
    base += (hash(gl_FragCoord.xy) - 0.5) * 0.026;
    gl_FragColor = vec4(base * (1.0 - uDim * 0.1), 1.0);
  }
`;

const CROSS_VERTEX = /* glsl */ `
  attribute vec2 aOrigin;
  uniform vec2 uHalf;
  void main() {
    gl_Position = projectionMatrix * modelViewMatrix *
      vec4(aOrigin * uHalf + position.xy, position.z, 1.0);
  }
`;

const CROSS_FRAGMENT = /* glsl */ `
  uniform float uDim;
  void main() { gl_FragColor = vec4(0.957, 0.945, 0.902, 0.34 * (1.0 - uDim * 0.8)); }
`;

function bubbleGeometry(model) {
  const geometry = new BufferGeometry();
  const add = (name, array, size) => geometry.setAttribute(name, new BufferAttribute(array, size));
  add("position", model.pos, 3);
  add("aOrigin", model.origin, 2);
  add("aBirth", model.birth, 1);
  add("aS", model.s, 1);
  add("aKind", model.kind, 1);
  add("aSettle", model.settle, 1);
  add("aOutcome", model.outcome, 1);
  add("aOrder", model.order, 1);
  add("aSeed", model.seed, 1);
  add("aSize", model.size, 1);
  return geometry;
}

// Fiducials: crosses etched on the chamber's front and back glass. Two depths, so they shift against
// each other when the camera moves, which is how stereo film was measured.
function crossGeometry() {
  const position = [];
  const origin = [];
  const arm = 0.022;
  for (let row = -1; row <= 1; row += 1) {
    for (let col = -2; col <= 2; col += 1) {
      const depth = (row + col) % 2 === 0 ? 0.32 : -0.42;
      const ox = col * 0.46;
      const oy = row * 0.62;
      position.push(-arm, 0, depth, arm, 0, depth, 0, -arm, depth, 0, arm, depth);
      for (let i = 0; i < 4; i += 1) origin.push(ox, oy);
    }
  }
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new BufferAttribute(new Float32Array(position), 3));
  geometry.setAttribute("aOrigin", new BufferAttribute(new Float32Array(origin), 2));
  return geometry;
}

export function createPlate(canvas, model) {
  const renderer = new WebGLRenderer({ canvas, antialias: false, alpha: false });
  renderer.autoClear = true;
  const scene = new Scene();
  const camera = new PerspectiveCamera(30, 1, 0.05, 20);
  const projected = new Vector3();
  const shared = { uHalf: { value: [1, 1] }, uDim: { value: 0 } };
  let local = 1;
  let ratio = 1;

  const ground = new Mesh(new PlaneGeometry(2, 2), new ShaderMaterial({
    vertexShader: GROUND_VERTEX,
    fragmentShader: GROUND_FRAGMENT,
    uniforms: { uHalf: shared.uHalf, uDim: shared.uDim },
    depthTest: false,
    depthWrite: false,
  }));
  ground.frustumCulled = false;
  ground.renderOrder = -2;
  scene.add(ground);

  const crosses = new LineSegments(crossGeometry(), new ShaderMaterial({
    vertexShader: CROSS_VERTEX,
    fragmentShader: CROSS_FRAGMENT,
    uniforms: { uHalf: shared.uHalf, uDim: shared.uDim },
    transparent: true,
    depthTest: false,
    depthWrite: false,
  }));
  crosses.frustumCulled = false;
  crosses.renderOrder = -1;
  scene.add(crosses);

  const material = new ShaderMaterial({
    vertexShader: BUBBLE_VERTEX,
    fragmentShader: BUBBLE_FRAGMENT,
    uniforms: {
      uTime: { value: 0 },
      uSize: { value: 2 },
      uLocal: { value: 1 },
      uDim: shared.uDim,
      uKeep: { value: 1 },
      uBranch: { value: 1.2 },
      uHalf: shared.uHalf,
      uFocusA: { value: model.focus.a.index },
      uFocusB: { value: model.focus.b.index },
    },
    transparent: true,
    depthTest: false,
    depthWrite: false,
  });
  const bubbles = new Points(bubbleGeometry(model), material);
  bubbles.frustumCulled = false;
  scene.add(bubbles);

  function resize(width, height, pixelRatio) {
    ratio = pixelRatio;
    renderer.setPixelRatio(pixelRatio);
    renderer.setSize(width, height, false);
    camera.aspect = width / height;
    camera.updateProjectionMatrix();
    shared.uHalf.value = [width / height, 1];
    local = width < height ? 0.72 : 1;
    material.uniforms.uLocal.value = local;
    material.uniforms.uBranch.value = width < height ? 1.7 : 1.2;
    // Sized from the spacing of the bubbles on screen, so a track stays a string of bubbles.
    material.uniforms.uSize.value = SPACING * local * (height / 2) * pixelRatio * 0.95;
  }

  return {
    resize,
    setQuality(pixelRatio, width, height) {
      resize(width, height, pixelRatio);
    },
    get pixelRatio() {
      return ratio;
    },
    worldOf(fork) {
      const [halfX, halfY] = shared.uHalf.value;
      return [fork.ox * halfX + fork.at.x * local, fork.oy * halfY + fork.at.y * local, fork.at.z * local];
    },
    project(point, width, height) {
      projected.set(point[0], point[1], point[2]).project(camera);
      return [(projected.x + 1) * 0.5 * width, (1 - projected.y) * 0.5 * height];
    },
    begun(fork) {
      const [halfX, halfY] = shared.uHalf.value;
      return [fork.ox * halfX + fork.at.began.x * local, fork.oy * halfY + fork.at.began.y * local];
    },
    render(time, dim, keep, view) {
      material.uniforms.uTime.value = time;
      material.uniforms.uKeep.value = keep;
      shared.uDim.value = dim;
      camera.position.set(view.x + view.tiltX, view.y + view.tiltY, DISTANCE / view.zoom);
      camera.lookAt(view.x, view.y, 0);
      camera.updateMatrixWorld();
      renderer.render(scene, camera);
    },
  };
}
