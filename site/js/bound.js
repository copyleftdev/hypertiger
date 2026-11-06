// The bound beside the truth, one small multiple per market on one shared scale.
const SVG = "http://www.w3.org/2000/svg";
const ROW = 176;
const GAP = 30;
const LEFT = 42;
const RIGHT = 12;
const TOP = 10;

function node(name, attributes, parent) {
  const element = document.createElementNS(SVG, name);
  for (const [key, value] of Object.entries(attributes)) element.setAttribute(key, value);
  parent?.appendChild(element);
  return element;
}

// A closed shape between two series, stepped, because a position does not move between samples.
function bandPath(rows, upper, lower, x, y) {
  let path = "";
  rows.forEach((row, index) => {
    const px = x(row[0]).toFixed(1);
    const py = y(row[upper]).toFixed(1);
    path += index === 0 ? `M${px} ${py}` : `H${px}V${py}`;
  });
  for (let index = rows.length - 1; index >= 0; index -= 1) {
    const px = x(rows[index][0]).toFixed(1);
    const py = y(rows[index][lower]).toFixed(1);
    path += index === rows.length - 1 ? `V${py}` : `V${py}H${px}`;
  }
  return `${path}Z`;
}

// One column per pixel at most, keeping each column's extremes. A narrow chart then shows the same
// outermost reach, and the truth can never appear to pass the bound through resampling alone.
function fitToWidth(rows, columns) {
  const size = Math.ceil(rows.length / columns);
  if (size <= 1) return rows;
  const fitted = [];
  for (let start = 0; start < rows.length; start += size) {
    const part = rows.slice(start, start + size);
    fitted.push([
      part[0][0],
      Math.max(...part.map((row) => row[1])),
      Math.min(...part.map((row) => row[2])),
      Math.max(...part.map((row) => row[3])),
      Math.min(...part.map((row) => row[4])),
    ]);
  }
  return fitted;
}

function draw(root, data, state) {
  const width = Math.max(root.clientWidth, 280);
  const coins = ["BTC", "ETH"];
  const all = coins.flatMap((coin) => data.bounds[coin]);
  const reach = Math.ceil(Math.max(...all.flatMap((row) => [row[3], -row[4]])) / 10) * 10;
  const startMs = all[0][0];
  const endMs = all[all.length - 1][0];
  const x = (ms) => LEFT + ((ms - startMs) / (endMs - startMs)) * (width - LEFT - RIGHT);
  const height = TOP + coins.length * ROW + GAP * (coins.length - 1) + 22;

  root.replaceChildren();
  const svg = node("svg", { width, height, viewBox: `0 0 ${width} ${height}`, role: "img",
    "aria-label": "Two charts over the 120 simulated seconds of seed 23, one each for BTC and ETH. The engine's bound stays at or outside the position that could truly be reached, at every sample." }, root);

  coins.forEach((coin, slot) => {
    const top = TOP + slot * (ROW + GAP);
    const y = (value) => top + ROW / 2 - (value / reach) * (ROW / 2);
    const rows = fitToWidth(data.bounds[coin], Math.floor((width - LEFT - RIGHT) / 2));
    for (const value of [-reach, 0, reach]) {
      node("line", { x1: LEFT, x2: width - RIGHT, y1: y(value), y2: y(value), class: value === 0 ? "axis" : "grid" }, svg);
      const tick = node("text", { x: LEFT - 8, y: y(value) + 4, "text-anchor": "end", class: "tick" }, svg);
      tick.textContent = value === 0 ? "0" : `${value > 0 ? "+" : "−"}${Math.abs(value)}`;
    }
    node("path", { d: bandPath(rows, 3, 4, x, y), class: "bound-area" }, svg);
    node("path", { d: bandPath(rows, 1, 2, x, y), class: "truth-area" }, svg);
    // The edge goes on top, so a bound equal to the truth reads as white edged in yellow.
    node("path", { d: bandPath(rows, 3, 4, x, y), class: "bound-edge" }, svg);
    const label = node("text", { x: width - RIGHT, y: top + 22, "text-anchor": "end", class: "coin" }, svg);
    label.textContent = coin;
  });

  const axis = TOP + coins.length * ROW + GAP * (coins.length - 1) + 16;
  for (let second = 20; second * 1000 < endMs; second += 20) {
    const tick = node("text", { x: x(second * 1000), y: axis, "text-anchor": "middle", class: "tick" }, svg);
    tick.textContent = `${second} s`;
  }

  const cross = node("line", { y1: 0, y2: height - 22, class: "cross", visibility: "hidden" }, svg);
  state.seek = (ms) => {
    const clamped = Math.min(endMs, Math.max(startMs, ms));
    state.ms = clamped;
    cross.setAttribute("x1", x(clamped));
    cross.setAttribute("x2", x(clamped));
    cross.setAttribute("visibility", "visible");
    state.read(clamped);
  };
  state.fromPixel = (px) => startMs + ((px - LEFT) / (width - LEFT - RIGHT)) * (endMs - startMs);
}

function reader(data, output) {
  const nearest = (rows, ms) => rows.reduce((best, row) => (row[0] <= ms ? row : best), rows[0]);
  const sized = (value) => `${value > 0 ? "+" : ""}${value.toFixed(3)}`;
  return (ms) => {
    const parts = ["BTC", "ETH"].map((coin) => {
      const row = nearest(data.bounds[coin], ms);
      return `${coin}: could be ${sized(row[2])} to ${sized(row[1])}, engine counts ${sized(row[4])} to ${sized(row[3])}`;
    });
    output.textContent = `${(ms / 1000).toFixed(2)} s. ${parts.join(". ")}.`;
  };
}

function fillTable(table, data) {
  const body = table.querySelector("tbody");
  for (const coin of ["BTC", "ETH"]) {
    const rows = data.bounds[coin];
    for (let i = 0; i < rows.length; i += 40) {
      const tr = document.createElement("tr");
      const row = rows[i];
      for (const cell of [coin, (row[0] / 1000).toFixed(2), row[2], row[1], row[4], row[3]]) {
        const td = document.createElement("td");
        td.textContent = cell;
        tr.appendChild(td);
      }
      body.appendChild(tr);
    }
  }
}

export function mountBound(root, output, table, data) {
  const state = { read: reader(data, output), ms: 0 };
  const redraw = () => draw(root, data, state);
  redraw();
  fillTable(table, data);
  new ResizeObserver(redraw).observe(root);
  root.addEventListener("pointermove", (event) => {
    const box = root.getBoundingClientRect();
    state.seek(state.fromPixel(event.clientX - box.left));
  });
  root.addEventListener("keydown", (event) => {
    const step = event.key === "ArrowRight" ? 1000 : event.key === "ArrowLeft" ? -1000 : 0;
    if (!step) return;
    event.preventDefault();
    state.seek((state.ms || data.bounds.BTC[0][0]) + step);
  });
}
