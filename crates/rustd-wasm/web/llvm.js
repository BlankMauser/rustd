// A page that builds one demo with one producer, draws it, and says what that cost:
//
//     llvm.html?demo=mandelbrot                           the hand-written lowering
//     llvm.html?demo=mandelbrot&backend=llvm&opt=1-fast   LLVM, after `build-llvm.sh`
//
// `compiler=<directory>` names the build of the compiler: `pkg`, or `pkg-llvm` for LLVM, if
// not given. The compiler with LLVM in it has the hand-written lowering too.
//
// `bench.mjs` reads `window.result` off it.

import { Program } from "./program.js";

const parameters = new URL(location.href).searchParams;
const demo = parameters.get("demo") ?? "mandelbrot";
const backend = parameters.get("backend") ?? "cranelift";
const opt = parameters.get("opt") ?? "0";
const llvm = backend === "llvm";
const compiler = parameters.get("compiler") ?? (llvm ? "pkg-llvm" : "pkg");
// How many frames the timing of the program is over.
const FRAMES = Number(parameters.get("frames") ?? 300);
// One edit of each demo that changes what a function or two compile to.
const EDITS = {
  mandelbrot: ["ITERATIONS: u32 = 60", "ITERATIONS: u32 = 80"],
  balls: ["RADIUS: f64 = 14.0", "RADIUS: f64 = 30.0"],
};

const producer = backend === "llvm" ? `LLVM, RUSTD_OPT=${opt}` : "the hand-written lowering";
document.getElementById("title").textContent = `${demo}, built by ${producer}`;
document.getElementById("links").innerHTML = ["mandelbrot", "balls", "fountain", "life", "sort"]
  .flatMap((demo) => [
    `<a href="?demo=${demo}">${demo}</a>`,
    `<a href="?demo=${demo}&backend=llvm&opt=0">O0</a>`,
    `<a href="?demo=${demo}&backend=llvm&opt=1-fast">O1-fast</a>`,
  ])
  .join("");

const canvas = document.getElementById("canvas");
const context = canvas.getContext("2d");
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

const worker = new Worker(`worker.js?compiler=${compiler}&backend=${backend}&opt=${opt}`, {
  type: "module",
});
const replies = [];
let waiting = null;
worker.onmessage = ({ data }) => (waiting ? waiting(data) : replies.push(data));
worker.onerror = (event) => fail(event.message);
const reply = () =>
  replies.length > 0
    ? Promise.resolve(replies.shift())
    : new Promise((resolve) => {
        waiting = (data) => {
          waiting = null;
          resolve(data);
        };
      });

function fail(why) {
  document.getElementById("numbers").textContent = `failed: ${why}`;
  window.result = { error: String(why) };
}

let sent = 0;
async function build(text, fresh) {
  const began = performance.now();
  worker.postMessage({ id: ++sent, text, fresh });
  const built = await reply();
  if (built.error) {
    throw new Error(built.error);
  }
  return { ...built, round_trip_ms: performance.now() - began };
}

// The program on its own, drawing nothing: how long its frames take.
function time(module, report) {
  const nothing = () => {};
  const timed = new Program({ fill: nothing, rect: nothing, circle: nothing, text: nothing });
  timed.apply(module, report);
  for (let frame = 0; frame < 20; frame++) {
    timed.call("click", 40 + 17 * frame, 30 + 11 * frame);
  }
  const began = performance.now();
  for (let frame = 0; frame < FRAMES; frame++) {
    timed.call("frame", 400, 300, 0.016);
  }
  return (performance.now() - began) / FRAMES;
}

const summary = ({ report, module, round_trip_ms }) => ({
  build_ms: round_trip_ms,
  analysis_ms: report.listing_ms,
  codegen_ms: report.codegen_ms,
  link_ms: report.link_ms,
  functions: report.functions.length,
  module_bytes: module.length,
  skipped: report.skipped.length,
  compiler_memory_bytes: report.compiler_bytes,
});

try {
  const loaded = await reply();
  if (loaded.error) {
    throw new Error(loaded.error);
  }
  const source = await (await fetch(`demos/${demo}.rs`)).text();
  const first = await build(source, true);
  const frame_ms = time(first.module, first.report);
  program = new Program(host);
  program.apply(first.module, first.report);
  const result = { demo, compiler, backend, opt, loaded: loaded.loaded, first: summary(first), frame_ms };
  if (EDITS[demo]) {
    const edited = await build(source.replace(...EDITS[demo]), false);
    program.apply(edited.module, edited.report);
    result.edit = summary(edited);
  }
  document.getElementById("numbers").textContent = JSON.stringify(result, null, 2);
  window.result = result;

  canvas.addEventListener("click", (event) => program.call("click", event.offsetX, event.offsetY));
  let last = performance.now();
  const frame = (now) => {
    const scale = devicePixelRatio;
    canvas.width = 400 * scale;
    canvas.height = 300 * scale;
    context.setTransform(scale, 0, 0, scale, 0, 0);
    context.font = "14px ui-monospace, monospace";
    program.call("frame", 400, 300, Math.min((now - last) / 1000, 0.05));
    last = now;
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
} catch (error) {
  fail(error.stack ?? error);
}
