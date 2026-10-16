// The program the compiler builds, as it runs in a page: one memory, one function table and
// one stack pointer, and every build's module instantiated over them.

export class Program {
  // `host` has the functions the program declares in `extern` blocks.
  constructor(host) {
    this.memory = new WebAssembly.Memory({ initial: 32n, address: "i64" });
    this.table = new WebAssembly.Table({ initial: 1, element: "anyfunc" });
    this.stackPointer = new WebAssembly.Global({ value: "i64", mutable: true }, 0n);
    this.exports = {};
    // A function nobody provides fails when it is called, not when the module is linked.
    this.host = new Proxy(host, {
      get: (host, name) =>
        host[name] ??
        (() => {
          throw new Error(`the program calls \`${String(name)}\`, which the page does not provide`);
        }),
      has: () => true,
    });
  }

  // Instantiates the module of one build. `report` is the build's JSON, parsed.
  apply(module, report) {
    if (this.table.length < report.table_len) {
      this.table.grow(report.table_len - this.table.length);
    }
    const pages = BigInt(Math.ceil(report.memory_len / 65536));
    const have = BigInt(this.memory.buffer.byteLength / 65536);
    if (have < pages) {
      this.memory.grow(pages - have);
    }
    if (this.stackPointer.value === 0n) {
      this.stackPointer.value = BigInt(report.stack_top);
    }
    const env = { memory: this.memory, table: this.table, stack_pointer: this.stackPointer };
    new WebAssembly.Instance(new WebAssembly.Module(module), { env, host: this.host });
    Object.assign(this.exports, report.exports);
  }

  // Calls a function of the program by symbol, through its slot.
  call(name, ...args) {
    const slot = this.exports[name];
    if (slot === undefined) {
      throw new Error(`the program has no function \`${name}\``);
    }
    return this.table.get(slot)(...args);
  }

  // The bytes at a pointer the program handed over.
  bytes(pointer, length) {
    return new Uint8Array(this.memory.buffer, Number(pointer), Number(length));
  }

  text(pointer, length) {
    return new TextDecoder().decode(this.bytes(pointer, length));
  }
}
