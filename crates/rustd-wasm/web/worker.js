// The compiler runs here, off the page's thread: the first build takes seconds.

import { Compiler } from "./compiler.js";

const started = performance.now();
const [wasm, core] = await Promise.all(
  ["pkg/rustd_wasm.wasm", "pkg/core.bundle"].map(async (url) => {
    const response = await fetch(url);
    if (!response.ok) {
      throw new Error(`${url}: ${response.status}`);
    }
    return new Uint8Array(await response.arrayBuffer());
  }),
);
const fetched = performance.now();
const compiled = await WebAssembly.compile(wasm);
let compiler = await Compiler.load(compiled, core);
postMessage({
  loaded: {
    wasm_bytes: wasm.length,
    core_bytes: core.length,
    fetch_ms: fetched - started,
    start_ms: performance.now() - fetched,
  },
});

// A `fresh` build is of another program: nothing of the one before is kept.
onmessage = async ({ data: { id, text, fresh } }) => {
  const start = performance.now();
  try {
    if (fresh) {
      compiler = await Compiler.load(compiled, core);
    }
    compiler.setFile("/app/main.rs", text);
    const { module, report } = compiler.build();
    report.build_ms = performance.now() - start;
    report.compiler_bytes = compiler.memoryBytes();
    postMessage({ id, module, report }, [module.buffer]);
  } catch (error) {
    postMessage({ id, error: String(error.message ?? error) });
  }
};
