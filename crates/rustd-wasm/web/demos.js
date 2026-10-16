// What both pages have in common: the list of demos, and the line that says how a build went.

export const demos = {
  balls: "bouncing balls",
  life: "game of life",
  sort: "sorting, with trait objects",
  mandelbrot: "mandelbrot set",
  dioxus: "Dioxus counter",
};

// Fills the picker. The Dioxus demo is a page of its own, since it runs inside the module of
// Dioxus that rustc built; `show` is called for a demo of the page that is open.
export function picker(current, show) {
  const picker = document.getElementById("demo");
  for (const [name, title] of Object.entries(demos)) {
    picker.add(new Option(title, name));
  }
  picker.value = current;
  picker.addEventListener("change", () => {
    const name = picker.value;
    if (name === "dioxus") {
      location.href = "dioxus.html";
    } else if (show) {
      show(name);
    } else {
      location.href = `index.html#${name}`;
    }
  });
  return picker;
}

const status = document.getElementById("status");
const state = document.getElementById("state");

// `kind` is `building`, `running` or `error`.
export function say(text, kind) {
  status.textContent = text;
  status.classList.toggle("error", kind === "error");
  state.textContent = kind;
  state.className = kind;
}

// Light or dark: what was picked before, or else what the system has. The editor hears of a
// change through the `rustd-theme` event.
const root = document.documentElement;
const toggle = document.getElementById("theme");
function theme(name) {
  root.dataset.theme = name;
  toggle.textContent = name === "light" ? "dark mode" : "light mode";
  window.dispatchEvent(new Event("rustd-theme"));
}
theme(localStorage.getItem("rustd-theme") ?? (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark"));
toggle.addEventListener("click", () => {
  const name = root.dataset.theme === "light" ? "dark" : "light";
  localStorage.setItem("rustd-theme", name);
  theme(name);
});
