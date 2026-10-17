// Measures the page in Chrome: `node bench.mjs [runs]`, with `python3 -m http.server 8377`
// serving this directory and `playwright-core` installed. Every run is a new browser, so
// nothing of the compiler is cached. Prints a line of JSON for each run.

import { chromium } from "playwright-core";

const runs = Number(process.argv[2] ?? 5);
const demos = (process.env.DEMOS ?? "mandelbrot,balls,fountain,life,sort").split(",");
// A producer, the level LLVM optimizes at, and the build of the compiler if not the usual.
const producers = (
  process.env.PRODUCERS ?? "cranelift,cranelift::pkg-llvm,llvm:0,llvm:1-fast"
).split(",");
const executablePath = process.env.CHROME ?? "/home/ubuntu/.local/bin/google-chrome";

for (const demo of demos) {
  for (const producer of producers) {
    const [backend, opt, compiler] = producer.split(":");
    for (let run = 0; run < runs; run++) {
      const browser = await chromium.launch({ executablePath, headless: true });
      const page = await browser.newPage();
      const said = [];
      page.on("console", (message) => said.push(message.text()));
      const query = [`demo=${demo}`, `backend=${backend}`];
      if (opt) query.push(`opt=${opt}`);
      if (compiler) query.push(`compiler=${compiler}`);
      await page.goto(`http://localhost:8377/llvm.html?${query.join("&")}`);
      await page.waitForFunction(() => window.result, null, { timeout: 120000 });
      const result = await page.evaluate(() => window.result);
      result.said = said.filter((line) => line.startsWith("backend:"));
      if (process.env.SHOT && run === 0) {
        await page.waitForTimeout(500);
        await page.screenshot({ path: `${process.env.SHOT}/${demo}-${producer.replaceAll(":", "-")}.png` });
      }
      console.log(JSON.stringify({ run, version: browser.version(), ...result }));
      await browser.close();
    }
  }
}
