// The playground: the editor, the compiler in a worker, and the program it builds.
//
// Everything of the compiler is under `rustd/`, put there by
// `crates/rustd-wasm/web/build-site.sh`. Nothing of it is asked for until the page is shown.

const PROGRAMS = [
  ["balls", "Bouncing balls"],
  ["fountain", "Fountain"],
  ["life", "Game of Life"],
  ["sort", "Sorting"],
  ["mandelbrot", "Mandelbrot set"],
];
// How long after a key the program is built again.
const DELAY = 150;

const $ = (id) => document.getElementById(id);
const [source, canvas, cover, coverText, bar, state, log, picker] =
  ["source", "canvas", "cover", "cover-text", "progress", "state", "log", "demo"].map($);
const context = canvas.getContext("2d");

const ms = (value) => (value < 1000 ? `${value.toFixed(0)} ms` : `${(value / 1000).toFixed(1)} s`);
const mb = (bytes) => `${(bytes / 1e6).toFixed(1)} MB`;
const count = (n, what) => `${n} ${what}${n === 1 ? "" : "s"}`;

function say(kind, text) {
  state.dataset.kind = kind;
  state.textContent = text;
}

// A line of the builds: `level` is `info`, `good` or `bad`, `notes` longer text under it.
function line(level, head, parts = [], notes = []) {
  const newest = log.scrollHeight - log.scrollTop - log.clientHeight < 8;
  const item = document.createElement("li");
  item.className = level;
  const time = document.createElement("time");
  time.textContent = new Date().toLocaleTimeString("en-GB");
  const bold = document.createElement("b");
  bold.textContent = head;
  const rest = document.createElement("span");
  rest.textContent = parts.map((it) => ` · ${it}`).join("");
  item.append(time, bold, rest);
  for (const note of notes) {
    const text = document.createElement("p");
    text.textContent = note;
    item.append(text);
  }
  log.append(item);
  if (newest) {
    log.scrollTop = log.scrollHeight;
  }
}

function unavailable(why) {
  coverText.textContent =
    "The playground is not available right now: the compiler was not built for this copy of the site.";
  bar.hidden = true;
  say("error", "unavailable");
  line("bad", "unavailable", [], [why]);
}

for (const [name, title] of PROGRAMS) {
  picker.add(new Option(title, name));
}
const named = location.hash.slice(1);
picker.value = PROGRAMS.some(([name]) => name === named) ? named : PROGRAMS[0][0];

const opened = performance.now();
let manifest;
let Program;
try {
  const response = await fetch("rustd/pkg/manifest.json", { cache: "no-cache" });
  if (!response.ok) {
    throw new Error(`rustd/pkg/manifest.json: ${response.status}`);
  }
  manifest = await response.json();
  ({ Program } = await import(`./rustd/program.js?${manifest.build}`));
} catch (error) {
  unavailable(String(error.message ?? error));
  throw error;
}
$("size-compiler").textContent = mb(manifest.compiler_bytes);
$("size-transfer").textContent = mb(manifest.transfer_bytes);

// The source of each program as it was last left, and as it came.
const texts = {};
const originals = {};
async function original(name) {
  if (!(name in originals)) {
    const response = await fetch(`rustd/demos/${name}.rs?${manifest.build}`);
    if (!response.ok) {
      throw new Error(`rustd/demos/${name}.rs: ${response.status}`);
    }
    originals[name] = await response.text();
  }
  return originals[name];
}

// What the program is given to draw with.
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
let program = new Program(host);
// What a function of the program that trapped said: it stops until the next build.
let stopped = null;

// The builds: one at a time, and an edit made during a build is built after it.
let ready = false;
let building = false;
let edited = 0;
let sent = 0;
// Whether the next build is of another program, and the first edit that is of it.
let fresh = true;
let first = 0;
let started = 0;
let timer;
// Whether a program has been built since the page was opened.
let built = false;

function build() {
  if (!ready || building || sent === edited) {
    return;
  }
  building = true;
  sent = edited;
  worker.postMessage({ id: sent, text: source.value, fresh });
  fresh = false;
  started = performance.now();
  if (state.dataset.kind !== "running") {
    say("building", "building…");
  }
}

async function show(name) {
  history.replaceState(null, "", `#${name}`);
  source.value = texts[name] ?? (await original(name));
  source.disabled = false;
  program = new Program(host);
  stopped = null;
  fresh = true;
  edited += 1;
  first = edited;
  if (ready) {
    say("building", "building…");
  }
  build();
}

const worker = new Worker(`rustd/worker.js?gz&${manifest.build}`, { type: "module" });
worker.onerror = (event) => failed("the compiler failed to load", event.message ?? "");
worker.onmessage = ({ data }) => {
  if ("progress" in data) {
    bar.value = data.progress / manifest.transfer_bytes;
    coverText.textContent = `Downloading the compiler: ${mb(data.progress)} of ${mb(manifest.transfer_bytes)}`;
    if (data.progress >= manifest.transfer_bytes) {
      bar.removeAttribute("value");
      coverText.textContent = "Starting the compiler…";
    }
    return;
  }
  if (data.loaded) {
    const { wasm_bytes, core_bytes, fetch_ms, start_ms } = data.loaded;
    line("info", "compiler ready", [
      `downloaded ${mb(manifest.transfer_bytes)} in ${ms(fetch_ms)}`,
      `compiler ${mb(wasm_bytes)}`,
      `source of core ${mb(core_bytes)}`,
      `started in ${ms(start_ms)}`,
    ]);
    coverText.textContent = "Compiling the program and the parts of core it uses…";
    ready = true;
    build();
    return;
  }
  building = false;
  if (data.error) {
    failed("build failed", data.error);
  } else if (data.id >= first) {
    try {
      apply(data);
    } catch (error) {
      failed("the build did not load", error.message ?? error);
    }
  }
  build();
};

function failed(head, error) {
  line("bad", head, [], [String(error)]);
  say("error", head);
  if (!built) {
    coverText.textContent = `${head}. The details are in the list below.`;
    bar.hidden = true;
  }
}

function apply({ report, module }) {
  program.apply(module, report);
  stopped = null;
  const notes = [
    ...report.skipped.map(({ name, why }) => `not compiled: ${name}: ${why}`),
    ...(report.errors ?? []),
  ];
  const took = ms(performance.now() - started);
  const parts = [
    `${count(report.functions.length, "function")} compiled`,
    `${report.bodies} in the program`,
    `module ${module.length < 10240 ? `${module.length} B` : `${(module.length / 1024).toFixed(0)} KB`}`,
    `analysis ${ms(report.listing_ms)}`,
    `codegen ${ms(report.codegen_ms)}`,
    `link ${ms(report.link_ms)}`,
  ];
  if (!built) {
    built = true;
    cover.hidden = true;
    parts.push(`${ms(performance.now() - opened)} since the page opened`);
  }
  if (notes.length > 0) {
    line("bad", `built in part in ${took}`, parts, notes);
    say("error", "some of it did not compile; the last version that did is running");
  } else {
    line("good", `built in ${took}`, parts);
    say("running", `built in ${took} · ${count(report.functions.length, "function")}`);
  }
}

function run(name, ...args) {
  if (stopped !== null || !(name in program.exports)) {
    return;
  }
  try {
    program.call(name, ...args);
  } catch (error) {
    stopped = error;
    failed(`\`${name}\` stopped`, error.message ?? error);
  }
}

let last = performance.now();
function frame(now) {
  const scale = devicePixelRatio;
  const { clientWidth: width, clientHeight: height } = canvas;
  if (canvas.width !== Math.round(width * scale) || canvas.height !== Math.round(height * scale)) {
    canvas.width = Math.round(width * scale);
    canvas.height = Math.round(height * scale);
  }
  context.setTransform(scale, 0, 0, scale, 0, 0);
  context.font = "14px ui-monospace, monospace";
  run("frame", width, height, Math.min((now - last) / 1000, 0.05));
  last = now;
  requestAnimationFrame(frame);
}
requestAnimationFrame(frame);

canvas.addEventListener("click", (event) => run("click", event.offsetX, event.offsetY));
source.addEventListener("input", () => {
  texts[picker.value] = source.value;
  edited += 1;
  clearTimeout(timer);
  timer = setTimeout(build, DELAY);
});
// Tab is four spaces in the editor.
source.addEventListener("keydown", (event) => {
  if (event.key === "Tab" && !event.shiftKey) {
    event.preventDefault();
    source.setRangeText("    ", source.selectionStart, source.selectionEnd, "end");
    source.dispatchEvent(new InputEvent("input"));
  }
});
picker.addEventListener("change", () => show(picker.value));
$("reset").addEventListener("click", () => {
  delete texts[picker.value];
  show(picker.value);
});

say("building", "loading the compiler…");
bar.hidden = false;
coverText.textContent = `Downloading the compiler: ${mb(manifest.transfer_bytes)}`;
show(picker.value).catch((error) => failed("the program did not load", error.message ?? error));
