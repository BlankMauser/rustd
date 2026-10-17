// The little of WASI the compiler with LLVM in it asks of its host. LLVM is C++, built on
// the libc of wasi-sdk, which imports a file system it never opens a file of here: there is
// an environment, a clock, random bytes and somewhere for `stderr` to go, and nothing else.

const BADF = 8;
const NOSYS = 52;

// The imports of `wasi_snapshot_preview1` for `module`. `env` is the environment, as an
// object of strings; `memory` gives the instance's memory, once there is one.
export function wasi(module, env, memory) {
  const encoder = new TextEncoder();
  const variables = Object.entries(env).map(([name, value]) => encoder.encode(`${name}=${value}\0`));
  const view = () => new DataView(memory().buffer);
  const bytes = (pointer, length) => new Uint8Array(memory().buffer, pointer >>> 0, length >>> 0);
  let line = "";
  const provided = {
    environ_sizes_get(count, size) {
      view().setUint32(count, variables.length, true);
      view().setUint32(size, variables.reduce((sum, it) => sum + it.length, 0), true);
      return 0;
    },
    environ_get(pointers, buffer) {
      for (const variable of variables) {
        view().setUint32(pointers, buffer, true);
        bytes(buffer, variable.length).set(variable);
        pointers += 4;
        buffer += variable.length;
      }
      return 0;
    },
    clock_time_get(_clock, _precision, time) {
      view().setBigUint64(time, BigInt(Math.round(performance.now() * 1e6)), true);
      return 0;
    },
    random_get(buffer, length) {
      // In pieces: `getRandomValues` gives at most 65536 bytes, and none into shared memory.
      for (let at = 0; at < length; at += 65536) {
        const piece = new Uint8Array(Math.min(65536, length - at));
        crypto.getRandomValues(piece);
        bytes(buffer + at, piece.length).set(piece);
      }
      return 0;
    },
    // Whatever is written is said, a line at a time: a panic, or what the producers did.
    fd_write(_fd, vectors, count, written) {
      let total = 0;
      for (let at = 0; at < count; at++) {
        const pointer = view().getUint32(vectors + 8 * at, true);
        const length = view().getUint32(vectors + 8 * at + 4, true);
        line += new TextDecoder().decode(bytes(pointer, length).slice());
        total += length;
      }
      const lines = line.split("\n");
      line = lines.pop();
      lines.forEach((it) => console.error(it));
      view().setUint32(written, total, true);
      return 0;
    },
    // No directory is open before the program starts, and so no file can be.
    fd_prestat_get: () => BADF,
    fd_fdstat_get: () => BADF,
    sched_yield: () => 0,
    proc_exit(code) {
      throw new Error(`the compiler exited with ${code}`);
    },
  };
  const imports = {};
  for (const { module: from, name } of WebAssembly.Module.imports(module)) {
    if (from === "wasi_snapshot_preview1") {
      imports[name] = provided[name] ?? (() => NOSYS);
    }
  }
  return imports;
}
