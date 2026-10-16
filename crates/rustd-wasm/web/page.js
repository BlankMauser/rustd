// The page: the source on the left, built by the compiler in `worker.js` on every edit, and
// the program on the right, drawing to the canvas through the functions in `host`.

import { Program } from "./program.js";

const source = document.getElementById("source");
const canvas = document.getElementById("canvas");
const status = document.getElementById("status");
const context = canvas.getContext("2d");

const demos = {
  balls: "bouncing balls",
  life: "game of life",
  sort: "sorting, with trait objects",
  mandelbrot: "mandelbrot set",
};
const picker = document.getElementById("demo");
for (const [name, title] of Object.entries(demos)) {
  picker.add(new Option(title, name));
}

let program;
const host = {
  fill: (red, green, blue) => (context.fillStyle = `rgb(${red} ${green} ${blue})`),
  rect: (x, y, width, height) => context.fillRect(x, y, width, height),
  circle(x, y, radius) {
    context.beginPath();
    context.arc(x, y, radius, 0, 2 * Math.PI);
    context.fill();
  },
  text: (pointer, length, x, y) => context.fillText(program.text(pointer, length), x, y),
};
program = new Program(host);

const say = (text, error = false) => {
  status.textContent = text;
  status.classList.toggle("error", error);
};
const ms = (value) => `${value.toFixed(1)} ms`;

const worker = new Worker("worker.js", { type: "module" });
worker.onerror = (event) => say(`the compiler failed to load: ${event.message}`, true);
let loaded = "";
let sent = 0;
let edited = 0;
// One build at a time: an edit made during a build is built after it.
let building = false;
let ready = false;
let stopped = null;

function build() {
  if (!ready || building || sent === edited) {
    return;
  }
  building = true;
  sent = edited;
  worker.postMessage({ id: sent, text: source.value, fresh });
  fresh = false;
  started = performance.now();
}
let started = 0;
// Whether the next build is of another demo, to start over with.
let fresh = false;

worker.onmessage = ({ data }) => {
  if (data.loaded) {
    const { wasm_bytes, core_bytes, fetch_ms, start_ms } = data.loaded;
    loaded = `compiler ${(wasm_bytes / 2 ** 20).toFixed(1)} MiB, core source ${(core_bytes / 2 ** 20).toFixed(1)} MiB, fetched in ${ms(fetch_ms)}, started in ${ms(start_ms)}`;
    say(`${loaded}\nfirst build…`);
    ready = true;
    build();
    return;
  }
  building = false;
  if (data.id < shown) {
    // A build of the demo that was on before.
  } else if (data.error) {
    say(data.error, true);
  } else {
    const { report, module } = data;
    try {
      program.apply(module, report);
      stopped = null;
      const skipped = report.skipped.map(({ name, why }) => `\nnot compiled: ${name}: ${why}`);
      const errors = (report.errors ?? []).map((it) => `\n${it}`);
      say(
        `${loaded}\nedit to running: ${ms(performance.now() - started)} — ` +
          `${report.functions.length} functions swapped in a ${module.length} byte module, ` +
          `of ${report.bodies} in the program ` +
          `(analysis ${ms(report.listing_ms)}, codegen ${ms(report.codegen_ms)}, link ${ms(report.link_ms)}), ` +
          `compiler memory ${(report.compiler_bytes / 2 ** 20).toFixed(0)} MiB` +
          errors.join("") +
          skipped.join(""),
        errors.length + skipped.length > 0,
      );
    } catch (error) {
      say(String(error), true);
    }
  }
  build();
};

let timer;
source.addEventListener("input", () => {
  edited += 1;
  clearTimeout(timer);
  timer = setTimeout(build, 60);
});
source.addEventListener("keydown", (event) => {
  if (event.key === "Tab") {
    event.preventDefault();
    source.setRangeText("    ", source.selectionStart, source.selectionEnd, "end");
    source.dispatchEvent(new Event("input"));
  }
});

// Runs a function of the program. One that traps stops the program until the next build.
function run(name, ...args) {
  if (stopped !== null || !(name in program.exports)) {
    return;
  }
  try {
    program.call(name, ...args);
  } catch (error) {
    stopped = `\`${name}\` stopped: ${error}`;
    say(stopped, true);
  }
}

canvas.addEventListener("click", (event) => run("click", event.offsetX, event.offsetY));

let last = performance.now();
function frame(now) {
  const scale = devicePixelRatio;
  const { clientWidth: width, clientHeight: height } = canvas;
  if (canvas.width !== width * scale || canvas.height !== height * scale) {
    canvas.width = width * scale;
    canvas.height = height * scale;
  }
  context.setTransform(scale, 0, 0, scale, 0, 0);
  context.font = "14px ui-monospace, monospace";
  run("frame", width, height, Math.min((now - last) / 1000, 0.05));
  last = now;
  requestAnimationFrame(frame);
}

// Builds from before this edit are of another demo, and not shown.
let shown = 0;
async function show(name) {
  const response = await fetch(`demos/${name}.rs`);
  source.value = response.ok ? await response.text() : `// ${name}: ${response.status}`;
  program = new Program(host);
  stopped = null;
  fresh = true;
  edited += 1;
  shown = edited;
  location.hash = name;
  if (ready) {
    say(`${loaded}\nbuilding ${demos[name]}…`);
  }
  build();
}
picker.addEventListener("change", () => show(picker.value));

picker.value = location.hash.slice(1) in demos ? location.hash.slice(1) : "balls";
await show(picker.value);
requestAnimationFrame(frame);
