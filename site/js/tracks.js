// Turns one real simulator run into bubbles. The timing, side, size and outcome of every track come
// from plate.json. Where a track sits on the plate is arrangement, and only that is invented.

const SPACING = 0.0062;
export const MAIN = 0;
export const LANDED_WORLD = 1;
export const NEVER_WORLD = 2;
export const BAR = 3;

// The two orders the scroll story follows. They fork close together in the real run.
const FOCUS_A = 20720;
const FOCUS_B = 22320;
const FOCUS_PLACE = { [FOCUS_A]: [-0.06, 0.5], [FOCUS_B]: [0.27, 0.56] };

function prng(seed) {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let mixed = Math.imul(state ^ (state >>> 15), 1 | state);
    mixed = (mixed + Math.imul(mixed ^ (mixed >>> 7), 61 | mixed)) ^ mixed;
    return ((mixed ^ (mixed >>> 14)) >>> 0) / 4294967296;
  };
}

function clamp(value, low, high) {
  return Math.min(high, Math.max(low, value));
}

class Writer {
  constructor() {
    this.fields = { pos: [], origin: [], birth: [], s: [], kind: [], settle: [], outcome: [],
      order: [], seed: [], size: [] };
    this.count = 0;
  }

  push(x, y, z, track, birth, s, kind, seed) {
    const f = this.fields;
    f.pos.push(x, y, z);
    f.origin.push(track.ox, track.oy);
    f.birth.push(birth);
    f.s.push(s);
    f.kind.push(kind);
    f.settle.push(track.settle);
    f.outcome.push(track.outcome);
    f.order.push(track.index);
    f.seed.push(seed);
    f.size.push(track.size);
    this.count += 1;
  }

  finish() {
    const out = { count: this.count };
    for (const [name, values] of Object.entries(this.fields)) out[name] = new Float32Array(values);
    return out;
  }
}

// Walks a path in a magnetic field: the radius shrinks as the particle loses energy, and the side of
// the order sets which way it curls, as charge does.
function walk(writer, track, from, length, spec, random) {
  let { x, y, z, heading } = from;
  let radius = spec.radius;
  const steps = Math.max(2, Math.ceil(length / SPACING));
  const ds = length / steps;
  for (let step = 0; step <= steps; step += 1) {
    const s = step * ds;
    const birth = spec.t0 + (s / length) * (spec.t1 - spec.t0);
    writer.push(x, y, z, track, birth, s, spec.kind, random());
    heading += (spec.curl * ds) / radius;
    radius = Math.max(spec.radiusMin, radius * Math.exp(-spec.loss * ds));
    x += Math.cos(heading) * ds;
    y += Math.sin(heading) * ds;
    z += spec.pitch * ds;
  }
  return { x, y, z, heading };
}

function radiusFor(order) {
  return clamp(0.13 + 0.15 * Math.sqrt(order.s), 0.12, 0.62);
}

function plainTrack(writer, order, track, random) {
  const end = order.e ?? order.t + 500;
  const seconds = Math.max(0.02, (end - order.t) / 1000);
  const rejected = order.o === "Rejected";
  const filled = (order.z ?? 0) > 0;
  const length = rejected ? 0.035 + 0.03 * random() : clamp((filled ? 0.32 : 0.1) + 0.46 * Math.sqrt(seconds), 0.08, 1.9);
  walk(writer, track, track.start, length, {
    t0: order.t / 1000,
    t1: end / 1000,
    kind: MAIN,
    curl: order.b ? 1 : -1,
    radius: radiusFor(order) * (0.8 + 0.5 * random()),
    radiusMin: 0.012,
    loss: filled ? 4.2 : 0.7,
    pitch: track.pitch,
  }, random);
}

function forkedTrack(writer, order, track, random) {
  const curl = order.b ? 1 : -1;
  const landed = order.o !== "NeverLanded";
  const doubt = order.d / 1000;
  const deadline = order.x / 1000;
  const settled = order.v / 1000;
  const mainLength = clamp(0.18 + 0.26 * Math.sqrt((order.d - order.t) / 1000), 0.14, 0.62);

  // The local origin is chosen so the fork sits exactly where the plate wants it.
  const probe = new Writer();
  const spec = {
    t0: order.t / 1000,
    t1: doubt,
    kind: MAIN,
    curl,
    radius: Math.max(0.42, radiusFor(order)),
    radiusMin: 0.2,
    loss: 0.25,
    pitch: track.pitch,
  };
  const replay = prng(track.index * 7 + 11);
  const end = walk(probe, track, track.start, mainLength, spec, replay);
  const shifted = { ...track.start, x: -end.x, y: -end.y, z: track.start.z };
  const fork = walk(writer, track, shifted, mainLength, spec, prng(track.index * 7 + 11));
  const began = { x: shifted.x, y: shifted.y };

  const speed = 0.17;
  const landedEnd = landed ? Math.max(order.e / 1000, settled + 0.5) : settled;
  const landedLength = Math.max(0.1, (landedEnd - doubt) * speed + (landed ? 0.14 : 0));
  walk(writer, track, { ...fork, heading: fork.heading + 0.34 * curl }, landedLength, {
    t0: doubt,
    t1: landedEnd,
    kind: LANDED_WORLD,
    curl,
    radius: 0.3,
    radiusMin: 0.016,
    loss: (order.z ?? 0) > 0 ? 3.4 : 1.2,
    pitch: track.pitch,
  }, random);

  const neverEnd = Math.max(deadline, doubt + 0.05);
  const neverLength = Math.max(0.17, (neverEnd - doubt) * speed * 1.4);
  const stub = walk(writer, track, { ...fork, heading: fork.heading - 0.4 * curl }, neverLength, {
    t0: doubt,
    t1: neverEnd,
    kind: NEVER_WORLD,
    curl: -curl,
    radius: 1.6,
    radiusMin: 1,
    loss: 0,
    pitch: 0,
  }, random);

  // The bar is the deadline itself, at the end of the world where the order never landed.
  const across = stub.heading + Math.PI / 2;
  for (let step = -5; step <= 5; step += 1) {
    const offset = step * SPACING;
    writer.push(stub.x + Math.cos(across) * offset, stub.y + Math.sin(across) * offset, stub.z,
      track, neverEnd, 0, BAR, random());
  }
  return { x: fork.x, y: fork.y, z: fork.z, began };
}

function place(order, index, random) {
  const pinned = order.d !== null && FOCUS_PLACE[order.t];
  if (pinned) return pinned;
  if (order.d !== null) return [(random() - 0.5) * 1.2, 0.08 + random() * 0.7];
  const upper = random() < 0.68;
  return [(random() - 0.5) * 2.0, upper ? random() * 0.98 : -random() * 0.98];
}

export function buildModel(data) {
  const writer = new Writer();
  const forks = [];
  data.orders.forEach((order, index) => {
    const random = prng(index * 2654435761 + data.seed);
    const [ox, oy] = place(order, index, random);
    const forked = order.d !== null && order.v !== null;
    const track = {
      index,
      ox,
      oy,
      settle: forked ? order.v / 1000 : 0,
      outcome: order.o === "NeverLanded" ? 0 : 1,
      size: order.r ? 0.8 : 1,
      pitch: (random() - 0.5) * 0.5,
      start: {
        x: 0,
        y: 0,
        z: (random() - 0.5) * 0.7,
        heading: random() * Math.PI * 2,
      },
    };
    if (!forked) {
      plainTrack(writer, order, track, random);
      return;
    }
    const at = forkedTrack(writer, order, track, random);
    forks.push({ order, index, ox, oy, at });
  });
  const focus = {
    a: forks.find((fork) => fork.order.t === FOCUS_A),
    b: forks.find((fork) => fork.order.t === FOCUS_B),
  };
  const lastBound = data.bounds.ETH[data.bounds.ETH.length - 1][0];
  return { ...writer.finish(), forks, focus, start: 5, end: lastBound / 1000, runSeed: data.seed };
}

// The same rules as the plate's vertex shader, for a browser with no WebGL.
export function drawFlat(context, width, height, model, time) {
  const halfX = width / height;
  const local = halfX < 1 ? 0.72 : 1;
  const unit = height / 2;
  context.clearRect(0, 0, width, height);
  for (let i = 0; i < model.count; i += 1) {
    if (model.birth[i] > time) continue;
    const kind = model.kind[i];
    const settled = time >= model.settle[i];
    const landedWorld = model.outcome[i] > 0.5;
    const winner = kind === MAIN || (kind === LANDED_WORLD ? landedWorld : !landedWorld);
    if (kind !== MAIN && settled && !winner) continue;
    if (kind !== MAIN && kind !== BAR && !settled && (model.s[i] / 0.05) % 1 > 0.55) continue;
    const age = time - model.birth[i];
    const alpha = kind === MAIN ? Math.max(0.1, 1 - age / 16) : Math.max(0.45, 1 - age / 30);
    let colour = "244,241,230";
    if (kind !== MAIN && !settled) colour = "255,200,0";
    if (kind !== MAIN && settled && !landedWorld) colour = "255,61,46";
    context.fillStyle = `rgba(${colour},${alpha})`;
    const x = (model.origin[i * 2] * halfX + model.pos[i * 3] * local + halfX) * unit;
    const y = (1 - (model.origin[i * 2 + 1] + model.pos[i * 3 + 1] * local)) * unit;
    context.fillRect(x - 0.9, y - 0.9, 1.8, 1.8);
  }
}
