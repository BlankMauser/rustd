// The Dioxus page. Dioxus and `std` are a module rustc built, `pkg/dioxus/base`, which runs
// here first. The compiler in `worker.js` builds the program on the left from the packed
// workspace, and each build is instantiated in that module's memory and table.

import { picker, say as tell } from "./demos.js";
import { Editor } from "./editor.js";
import init from "./pkg/dioxus/base.js";
import { Program } from "./program.js";

const say = (text, error = false) =>
  tell(text, error ? "error" : text.endsWith("…") ? "building" : "running");
picker("dioxus");
const ms = (value) => `${value.toFixed(1)} ms`;

// What the program may fill of the module it is linked into: slots at the end of its table,
// and memory its allocator has not been given.
const SLOTS = 1 << 17;
const BYTES = 64 << 20;

// The module keeps every import wasm-bindgen can ask for, among them the promise integration
// some browsers lack. Nothing here calls through it, so the plain function stands in.
WebAssembly.Suspending ??= function (inner) {
  return inner;
};

const base = await init();
const program = new Program({}, base);
const start = { table: program.table.grow(SLOTS), memory: program.memory.grow(BYTES >> 16) << 16 };

const worker = new Worker("worker.js?pack=pkg/dioxus.pack", { type: "module" });
worker.onerror = (event) => say(`the compiler failed to load: ${event.message}`, true);
let loaded = "";
let sent = -1;
let edited = 0;
let building = false;
let ready = false;
let started = 0;
let launched = false;

function build() {
  if (!ready || building || sent === edited) {
    return;
  }
  building = true;
  worker.postMessage({ id: edited, text: source.value, start: sent < 0 ? start : undefined });
  sent = edited;
  started = performance.now();
}

let timer;
const source = await Editor.load(document.getElementById("source"), () => {
  edited += 1;
  clearTimeout(timer);
  timer = setTimeout(build, 200);
});
source.value = await (await fetch("dioxus/app/src/main.rs")).text();

worker.onmessage = ({ data }) => {
  if (data.loaded) {
    const { wasm_bytes, core_bytes, fetch_ms, start_ms, analysis } = data.loaded;
    loaded = `compiler ${(wasm_bytes / 2 ** 20).toFixed(1)} MiB, workspace ${(core_bytes / 2 ** 20).toFixed(1)} MiB, fetched in ${ms(fetch_ms)}, loaded in ${ms(start_ms)} (${analysis})`;
    say(`${loaded}\nfirst build…`);
    ready = true;
    build();
    return;
  }
  building = false;
  if (data.error) {
    say(data.error, true);
  } else {
    const { report, module } = data;
    try {
      if (report.table_len > start.table + SLOTS || report.memory_len > start.memory + BYTES) {
        throw new Error("the program outgrew what the page set aside for it");
      }
      program.apply(module, report);
      if (!launched) {
        const app = Object.entries(report.exports).find(([name]) => name.endsWith("::App"));
        base.rustd_launch(app[1]);
        launched = true;
      } else {
        base.rustd_rerender();
      }
      const notes = [
        ...report.skipped.map(({ name, why }) => `\nnot compiled: ${name}: ${why}`),
        ...(report.mismatched ?? []).map((it) => `\nsignature differs from rustc's: ${it}`),
        ...(report.errors ?? []).map((it) => `\n${it}`),
      ];
      say(
        `${loaded}\nedit to running: ${ms(performance.now() - started)} — ` +
          `${report.functions.length} functions swapped in a ${module.length} byte module ` +
          `(codegen ${ms(report.codegen_ms)}, link ${ms(report.link_ms)}), ` +
          `${Object.keys(report.host ?? {}).length} functions of the rustc-built module called, ` +
          `compiler memory ${(report.compiler_bytes / 2 ** 20).toFixed(0)} MiB` +
          notes.join(""),
        notes.length > 0,
      );
    } catch (error) {
      say(String(error.stack ?? error), true);
    }
  }
  build();
};

