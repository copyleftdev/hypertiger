import { buildModel, drawFlat } from "./tracks.js";
import { mountBound } from "./bound.js";

const SPEED = 0.5; // Simulated seconds per real second.
const START_AT = 21.4; // Just before the two orders the page follows lose their replies.
const STILL_AT = 24.0; // The frame for reduced motion: both of those orders in doubt.
const COINS = ["BTC", "ETH"];
const WHY = {
  Delayed: "the request arrived late",
  Garbled: "the reply was cut short",
  ResetBeforeLanding: "the connection reset before it landed",
  ResetAfterLanding: "the connection reset after it landed",
  LostRequest: "the request was lost",
  LostReply: "the reply was lost",
  Normal: "no reply it could use",
};

// Scroll progress through the pinned section, against simulated time and what the camera looks at.
const KEYS = [
  { p: 0.0, t: 20.72, at: "flight", zoom: 2.0 },
  { p: 0.14, t: 22.0, at: "flight", zoom: 2.0 },
  { p: 0.28, t: 22.5, at: "a", zoom: 2.2 },
  { p: 0.42, t: 23.5, at: "a", zoom: 2.2 },
  { p: 0.56, t: 24.2, at: "mid", zoom: 2.0, ox: 0.3 },
  { p: 0.7, t: 24.95, at: "mid", zoom: 1.75, ox: 0.5, oy: 0.2 },
  { p: 0.84, t: 25.7, at: "mid", zoom: 1.75, ox: 0.5, oy: 0.2 },
  { p: 0.94, t: 26.05, at: "mid", zoom: 1.5, ox: 0.4, oy: 0.2 },
  { p: 1.0, t: 26.1, at: "mid", zoom: 1.5, ox: 0.4, oy: 0.2 },
];

const element = (id) => document.getElementById(id);
const motionQuery = matchMedia("(prefers-reduced-motion: reduce)");

function lerp(a, b, f) {
  return a + (b - a) * f;
}

function smooth(f) {
  return f * f * (3 - 2 * f);
}

function clock(seconds) {
  return `${seconds.toFixed(2).padStart(6, "0")} s`;
}

function tally(forks, time) {
  const counts = { doubt: 0, never: 0, landed: 0, counted: 0 };
  for (const { order } of forks) {
    const doubt = order.d / 1000;
    const settled = order.v / 1000;
    if (time < doubt) continue;
    if (time < settled) {
      counts.doubt += 1;
      counts.counted += order.s;
    } else if (order.o === "NeverLanded") {
      counts.never += 1;
    } else {
      counts.landed += 1;
    }
  }
  return counts;
}

function describe(order) {
  const side = order.b ? "buy" : "sell";
  const lines = [`${side} ${order.s} ${COINS[order.m]}`, `${WHY[order.f] ?? WHY.Normal}`];
  lines.push(`deadline ${(order.x / 1000).toFixed(2)} s`);
  return lines.join(" · ");
}

function setupCopy(button) {
  const status = button.parentElement.querySelector("[data-copy-status]");
  const source = button.parentElement.querySelector("code");
  button.addEventListener("click", async () => {
    const text = button.dataset.copy ?? source.textContent;
    try {
      await navigator.clipboard.writeText(text);
      button.textContent = "Copied";
      status.textContent = "Command copied.";
    } catch {
      const range = document.createRange();
      range.selectNodeContents(source);
      const selection = getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      button.textContent = "Press Ctrl+C";
      status.textContent = "Could not reach the clipboard. The command is selected, so press Ctrl+C.";
    }
    setTimeout(() => {
      button.textContent = "Copy";
    }, 2400);
  });
}

function drawTicks(canvas, model) {
  const ratio = Math.min(devicePixelRatio, 2);
  const width = canvas.clientWidth;
  const height = canvas.clientHeight;
  canvas.width = width * ratio;
  canvas.height = height * ratio;
  const context = canvas.getContext("2d");
  context.scale(ratio, ratio);
  const x = (seconds) => ((seconds - model.start) / (model.end - model.start)) * width;
  // Perforations, drawn so they stay square however wide the strip gets.
  context.fillStyle = "#f4f1e6";
  for (let at = 4; at < width; at += 19) {
    context.fillRect(at, 3, 7, 4);
    context.fillRect(at, height - 7, 7, 4);
  }
  const top = 11;
  const band = height - 22;
  for (const { order } of model.forks) {
    const from = x(order.d / 1000);
    const to = x(order.v / 1000);
    const never = order.o === "NeverLanded";
    context.fillStyle = "#ffc800";
    context.fillRect(from, top + band * 0.2, Math.max(1.5, to - from), band * 0.6);
    context.fillStyle = never ? "#ff3d2e" : "#f4f1e6";
    context.fillRect(to, never ? top + band * 0.2 : top, 2, never ? band * 0.6 : band);
  }
}

class Page {
  constructor(data) {
    this.data = data;
    this.model = buildModel(data);
    this.reduced = motionQuery.matches;
    this.time = this.reduced ? STILL_AT : START_AT;
    this.played = this.time;
    this.playing = !this.reduced;
    this.scrubbing = false;
    this.view = { x: 0, y: 0, zoom: 1, tiltX: 0, tiltY: 0 };
    this.pointer = [0, 0];
    this.dim = 0;
    this.keep = 1;
    this.visible = true;
    this.lastFrame = performance.now();
    this.lastHud = -1;
    this.slow = 0;
    this.pin = element("story");
    this.steps = [...this.pin.querySelectorAll("[data-t]")];
    this.tags = [...element("tags").querySelectorAll(".tag")];
    this.leaders = [...element("tags").querySelectorAll(".leader")];
    this.scrub = element("scrub");
    this.play = element("play");
    this.clockHero = element("clock");
    this.clockPin = element("story-clock");
    this.scan = element("scan");
    this.counts = {
      doubt: element("n-doubt"),
      never: element("n-never"),
      landed: element("n-landed"),
    };
    this.storyCounts = { doubt: element("s-doubt"), counted: element("s-counted") };
    this.hero = element("hero");
    this.type = element("type");
  }

  setupControls() {
    const { model, scrub } = this;
    scrub.min = model.start;
    scrub.max = model.end.toFixed(2);
    scrub.step = 0.25;
    scrub.addEventListener("input", () => {
      this.scrubbing = true;
      this.played = Number(scrub.value);
      this.drawNow();
    });
    const release = () => {
      this.scrubbing = false;
    };
    scrub.addEventListener("change", release);
    scrub.addEventListener("pointerup", release);
    this.play.addEventListener("click", () => this.setPlaying(!this.playing));
    this.setPlaying(this.playing);
    element("next").addEventListener("click", () => {
      const after = model.forks.find(({ order }) => order.d / 1000 > this.played + 0.8);
      const fork = after ?? model.forks[0];
      this.played = fork.order.d / 1000 - 0.7;
      this.drawNow();
    });
    drawTicks(element("ticks"), model);
    addEventListener("pointermove", (event) => {
      this.pointer = [event.clientX / innerWidth - 0.5, event.clientY / innerHeight - 0.5];
    }, { passive: true });
    const watch = new IntersectionObserver((entries) => {
      for (const entry of entries) entry.target.dataset.seen = entry.isIntersecting ? "1" : "";
      this.visible = [...document.querySelectorAll("[data-plate]")].some((s) => s.dataset.seen === "1");
      if (this.visible) this.requestFrame();
    });
    document.querySelectorAll("[data-plate]").forEach((section) => watch.observe(section));
    document.addEventListener("visibilitychange", () => this.requestFrame());
    addEventListener("resize", () => {
      this.resize();
      drawTicks(element("ticks"), model);
      this.measureType();
      this.drawNow();
    });
  }

  setPlaying(playing) {
    this.playing = playing;
    this.play.setAttribute("aria-label", playing ? "Pause the run" : "Play the run");
    this.play.dataset.state = playing ? "playing" : "paused";
    this.requestFrame();
  }

  measureType() {
    for (const [name, id] of [["--l2", "line2"], ["--l3", "line3"]]) {
      const right = element(id).getBoundingClientRect().right;
      const left = this.type.getBoundingClientRect().left;
      this.type.style.setProperty(name, `${Math.ceil(right - left)}px`);
    }
  }

  // Where the pinned section's scroll is, or null when it is not pinned.
  pinProgress() {
    if (this.reduced || !document.documentElement.classList.contains("pinned")) return null;
    const box = this.pin.getBoundingClientRect();
    const range = box.height - innerHeight;
    if (box.top > 0 || box.bottom < innerHeight) return null;
    return Math.min(1, Math.max(0, -box.top / range));
  }

  focusPoint(at) {
    const a = this.plate.worldOf(this.model.focus.a);
    const b = this.plate.worldOf(this.model.focus.b);
    if (at === "a") return a;
    if (at === "b") return b;
    if (at === "flight") {
      const began = this.plate.begun(this.model.focus.a);
      return [(a[0] + began[0]) / 2, (a[1] + began[1]) / 2];
    }
    return [(a[0] + b[0]) / 2, (a[1] + b[1]) / 2];
  }

  storyFrame(progress) {
    let index = 0;
    while (index < KEYS.length - 2 && progress > KEYS[index + 1].p) index += 1;
    const from = KEYS[index];
    const to = KEYS[index + 1];
    const f = smooth(Math.min(1, Math.max(0, (progress - from.p) / (to.p - from.p))));
    const near = this.focusPoint(from.at);
    const far = this.focusPoint(to.at);
    const wide = innerWidth > innerHeight;
    const zoom = lerp(from.zoom, to.zoom, f) * (wide ? 1 : 0.72);
    const offsetX = wide ? lerp(from.ox ?? 0.12, to.ox ?? 0.12, f) : 0;
    const offsetY = wide ? lerp(from.oy ?? 0.1, to.oy ?? 0.1, f) : 0.34;
    return {
      time: lerp(from.t, to.t, f),
      // Off centre, up and to the right, because the caption holds the lower left.
      x: lerp(near[0], far[0], f) - offsetX / zoom,
      y: lerp(near[1], far[1], f) - offsetY / zoom,
      zoom,
    };
  }

  // A caption changes on the moment it names, read from the same clock as the plate.
  showStep(time) {
    let active = this.steps[0];
    for (const step of this.steps) if (time >= Number(step.dataset.t)) active = step;
    for (const step of this.steps) step.classList.toggle("on", step === active);
  }

  advance(dt) {
    if (this.playing && !this.scrubbing && !this.reduced) {
      this.played += dt * SPEED;
      if (this.played > this.model.end) this.played = this.model.start + 1;
    }
    const progress = this.pinProgress();
    const ease = this.reduced ? 1 : 1 - Math.exp(-dt * 5);
    let target = { x: 0, y: 0, zoom: 1 };
    let dimTarget = 0;
    let keepTarget = 1;
    if (progress !== null) {
      const frame = this.storyFrame(progress);
      this.time = frame.time;
      target = frame;
      dimTarget = 1;
      this.showStep(frame.time);
    } else {
      this.time = this.played;
      // Text sits directly on the plate past the story, so the plate steps back behind it.
      const past = scrollY > this.hero.offsetHeight + this.pin.offsetHeight - innerHeight;
      const closing = element("close").getBoundingClientRect().top < innerHeight * 0.55;
      dimTarget = past && !closing ? 1 : 0;
      keepTarget = past && !closing ? 0 : 1;
    }
    this.keep = lerp(this.keep, keepTarget, ease);
    const view = this.view;
    view.x = lerp(view.x, target.x, ease);
    view.y = lerp(view.y, target.y, ease);
    view.zoom = lerp(view.zoom, target.zoom, ease);
    const tilt = this.reduced ? 0 : 0.2;
    view.tiltX = lerp(view.tiltX, this.pointer[0] * tilt, ease * 0.5);
    view.tiltY = lerp(view.tiltY, -this.pointer[1] * tilt, ease * 0.5);
    this.dim = lerp(this.dim, dimTarget, ease);
  }

  updateHud() {
    const stamp = Math.round(this.time * 10);
    if (stamp === this.lastHud) return;
    this.lastHud = stamp;
    const { forks } = this.model;
    const counts = tally(forks, this.time);
    this.counts.doubt.textContent = counts.doubt;
    this.counts.never.textContent = counts.never;
    this.counts.landed.textContent = counts.landed;
    this.clockHero.textContent = clock(this.time);
    this.clockPin.textContent = clock(this.time);
    this.storyCounts.doubt.textContent = counts.doubt;
    this.storyCounts.counted.textContent = counts.counted.toFixed(3);
    if (!this.scrubbing) this.scrub.value = this.played;
    const live = forks.filter(({ order }) => order.d / 1000 <= this.time && this.time < order.v / 1000);
    const latest = live[live.length - 1];
    this.scan.textContent = latest ? describe(latest.order) : "No order in doubt at this moment.";
  }

  updateTags() {
    const pinned = this.pinProgress() !== null;
    const { forks, focus } = this.model;
    const typeTop = this.type.getBoundingClientRect().top;
    const list = pinned ? [focus.a, focus.b] : forks;
    const limit = pinned ? 2 : innerWidth < 720 ? 1 : 3;
    const heroInView = scrollY < innerHeight * 0.5;
    const placed = [];
    // In a narrow pinned frame a tag at its fork would cover the track, so tags wait in the empty
    // plate above the caption, each with a leader to its own fork.
    const docked = pinned && innerWidth < innerHeight;
    let used = 0;
    for (const fork of list) {
      if (used >= limit || (!pinned && !heroInView) || !this.plate) break;
      const { order } = fork;
      const doubt = order.d / 1000;
      const settled = order.v / 1000;
      if (this.time < doubt || this.time > settled + 1.6 || settled - doubt < 0.3) continue;
      const [x, y] = this.plate.project(this.plate.worldOf(fork), innerWidth, innerHeight);
      if (!pinned && (y > typeTop - 18 || y < 90)) continue;
      if (x < 10 || x > innerWidth - 10 || y < 10 || y > innerHeight - 30) continue;
      const tag = this.tags[used];
      used += 1;
      const never = order.o === "NeverLanded";
      const state = this.time < settled ? "doubt" : never ? "never" : "landed";
      tag.hidden = false;
      tag.dataset.state = state;
      tag.firstElementChild.textContent = state === "doubt" ? (docked ? "in doubt" : "in doubt, counted in full")
        : state === "never" ? "never landed" : "landed after all";
      tag.lastElementChild.textContent = `${order.s} ${COINS[order.m]}`;
      if (docked) this.dockTag(tag, used - 1, x, y);
      else this.placeTag(tag, x, y, placed);
    }
    for (let i = docked ? used : 0; i < this.leaders.length; i += 1) this.leaders[i].hidden = true;
    for (let i = used; i < this.tags.length; i += 1) this.tags[i].hidden = true;
  }

  dockTag(tag, slot, forkX, forkY) {
    const caption = this.steps.find((step) => step.classList.contains("on"));
    const floor = caption.getBoundingClientRect().top - tag.offsetHeight - 14;
    const left = slot === 0 ? 16 : Math.max(16, innerWidth - tag.offsetWidth - 16);
    tag.dataset.flip = "1";
    tag.style.margin = "0";
    tag.style.transform = `translate(${Math.round(left)}px, ${Math.round(floor)}px)`;
    const fromX = left + tag.offsetWidth / 2;
    const run = forkX - fromX;
    const rise = forkY - floor;
    const leader = this.leaders[slot];
    leader.hidden = false;
    leader.style.width = `${Math.hypot(run, rise)}px`;
    leader.style.transform = `translate(${Math.round(fromX)}px, ${Math.round(floor)}px) rotate(${Math.atan2(rise, run)}rad)`;
  }

  // Opens to whichever side has room, and steps clear of a tag already placed.
  placeTag(tag, x, y, placed) {
    const width = tag.offsetWidth + 12;
    const height = tag.offsetHeight + 10;
    const flip = x + width > innerWidth - 8;
    tag.dataset.flip = flip ? "1" : "";
    tag.style.margin = "";
    const left = flip ? Math.max(8, x - width - 12) : x;
    let top = y;
    for (const other of placed) {
      const apart = left + width < other.left || other.left + other.width < left;
      if (!apart && Math.abs(top - other.top) < height) top = other.top + height;
    }
    placed.push({ left, top, width });
    tag.style.transform = `translate(${Math.round(left)}px, ${Math.round(top)}px)`;
  }

  adapt(dt) {
    // A weak GPU drops resolution rather than frames.
    this.slow = dt > 0.03 ? this.slow + 1 : Math.max(0, this.slow - 1);
    if (this.slow > 45 && this.plate.pixelRatio > 1) {
      this.slow = 0;
      this.plate.setQuality(1, innerWidth, innerHeight);
    }
  }

  frame(now) {
    this.pending = false;
    const dt = Math.min(0.1, (now - this.lastFrame) / 1000);
    this.lastFrame = now;
    if (!this.visible || document.hidden) return;
    this.advance(dt);
    this.updateHud();
    this.plate.render(this.time, this.dim, this.keep, this.view);
    this.updateTags();
    this.adapt(dt);
    this.requestFrame();
  }

  requestFrame() {
    if (this.pending || !this.plate) return;
    this.pending = true;
    this.lastFrame = Math.max(this.lastFrame, performance.now() - 100);
    requestAnimationFrame((now) => this.frame(now));
  }

  drawNow() {
    if (this.flat) {
      this.time = this.played;
      this.flat();
      this.updateHud();
      return;
    }
    this.requestFrame();
  }

  resize() {
    if (this.plate) this.plate.resize(innerWidth, innerHeight, Math.min(devicePixelRatio, 1.75));
  }

  useFlat(canvas) {
    const context = canvas.getContext("2d");
    this.flat = () => {
      const ratio = Math.min(devicePixelRatio, 2);
      canvas.width = innerWidth * ratio;
      canvas.height = innerHeight * ratio;
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
      drawFlat(context, innerWidth, innerHeight, this.model, this.time);
    };
    this.playing = false;
    this.play.hidden = true;
    this.time = STILL_AT;
    this.played = STILL_AT;
    this.flat();
    this.updateHud();
    addEventListener("resize", () => this.flat());
    // This plate has no shader to dim, so past the first plate the whole canvas steps back.
    const stepBack = () => {
      canvas.style.opacity = scrollY > innerHeight * 0.6 ? "0.1" : "1";
    };
    addEventListener("scroll", stepBack, { passive: true });
    stepBack();
  }
}

function canUseWebgl() {
  try {
    return Boolean(document.createElement("canvas").getContext("webgl2"));
  } catch {
    return false;
  }
}

async function main() {
  document.querySelectorAll("[data-copy-button]").forEach(setupCopy);
  const response = await fetch("plate.json");
  if (!response.ok) throw new Error(`plate.json: ${response.status}`);
  const data = await response.json();

  mountBound(element("chart"), element("chart-read"), element("chart-table"), data);
  const page = new Page(data);
  if (!page.reduced) document.documentElement.classList.add("pinned");
  page.setupControls();
  await document.fonts.ready;
  page.measureType();

  const canvas = element("plate");
  if (!canUseWebgl()) {
    document.documentElement.classList.remove("pinned");
    page.useFlat(canvas);
    return;
  }
  // Three.js arrives after the page can be read, so nothing waits on it.
  const { createPlate } = await import("./plate.js");
  page.plate = createPlate(canvas, page.model);
  page.resize();
  page.requestFrame();
  document.documentElement.classList.add("live");
}

main().catch((error) => {
  // The page reads fine without the plate, so a failure here costs the picture and nothing else.
  document.documentElement.classList.remove("pinned");
  console.error("The plate could not start:", error);
});
