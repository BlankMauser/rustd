// The page without a browser: `node check.mjs`, after `build.sh`. Builds the demo, edits it
// twice, and checks that what it draws follows the edits while its balls stay.

import assert from "node:assert";
import { readFileSync } from "node:fs";
import { Compiler } from "./compiler.js";
import { Program } from "./program.js";

const read = (name) => readFileSync(new URL(name, import.meta.url));
const compiler = await Compiler.load(read("pkg/rustd_wasm.wasm"), read("pkg/core.bundle"));
let calls = [];
const program = new Program({
  fill: (...color) => calls.push(["fill", ...color]),
  rect: (...rect) => calls.push(["rect", ...rect]),
  circle: (...circle) => calls.push(["circle", ...circle]),
  text: (pointer, length, x, y) => calls.push(["text", program.text(pointer, length), x, y]),
});

function build(source) {
  const start = performance.now();
  compiler.setFile("/app/main.rs", source);
  const { module, report } = compiler.build();
  program.apply(module, report);
  const took = (performance.now() - start).toFixed(1);
  console.log(
    `${took} ms: ${report.functions.length} functions in ${module.length} bytes, ` +
      `${report.compiled} of ${report.bodies} bodies compiled, ` +
      `compiler memory ${compiler.memoryBytes() >> 20} MiB`,
  );
  assert.deepEqual(report.skipped, []);
  calls = [];
  program.call("frame", 400, 300, 0.016);
  return { report, calls };
}

const source = read("demo.rs").toString();
const circles = (calls) => calls.filter(([name]) => name === "circle");

let { report, calls: drawn } = build(source);
assert.equal(circles(drawn).length, 0);
program.call("click", 100, 50);

const bigger = source.replace("RADIUS: f64 = 14.0", "RADIUS: f64 = 30.0");
({ report, calls: drawn } = build(bigger));
assert.ok(report.functions.length < 5, "only what uses the constant is compiled again");
assert.deepEqual(circles(drawn).map((it) => it[3]), [30], "the ball is still there, and bigger");

({ report, calls: drawn } = build(bigger.replace("click to drop a ball", "hello")));
assert.deepEqual(report.functions, ["frame"]);
assert.equal(drawn.at(-1)[1], "hello");
assert.equal(circles(drawn).length, 1);
console.log("ok");
