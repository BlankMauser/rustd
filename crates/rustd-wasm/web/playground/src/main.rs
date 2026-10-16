//! The page, a playground: a picker of programs, the source of one in an editor, what it
//! shows when it runs, and how its last build went.
//!
//! This app does not compile anything. The compiler is a worker, and `playground.js` runs it
//! and the programs it builds: this app tells it of the program to show and of every edit,
//! through `window.rustd`, and hears how builds went through [`rustd_status`].

use std::cell::RefCell;

use dioxus::prelude::*;
use dioxus_code::{CodeTheme, Theme};
use dioxus_code_editor::{CodeEditor, Language};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures_util::StreamExt;
use wasm_bindgen::prelude::*;

fn main() {
    dioxus::LaunchBuilder::web()
        .with_cfg(dioxus::web::Config::new().rootname("playground"))
        .launch(Playground);
}

#[component]
fn Playground() -> Element {
    let mut demo = use_signal(Demo::of_address);
    let mut source = use_signal(|| demo.peek().source.to_owned());
    let mut dark = use_signal(stored_dark);
    let mut status = use_signal(|| Status {
        kind: "building".to_owned(),
        text: "loading the compiler…".to_owned(),
    });

    // The program to show, once what it is shown in is in the page.
    use_effect(move || {
        let demo = demo();
        set_address(demo.name);
        show(demo.name, demo.source);
    });
    use_future(move || async move {
        let Some(mut receiver) = STATUSES.with_borrow_mut(|(_, receiver)| receiver.take()) else {
            return;
        };
        while let Some(next) = receiver.next().await {
            status.set(next);
        }
    });

    let shown = demo();
    let app = shown.output == Output::App;
    let theme = if dark() { Theme::TOKYO_NIGHT } else { Theme::GITHUB_LIGHT };
    let Status { kind, text } = status();
    rsx! {
        div { class: "playground", "data-theme": if dark() { "dark" } else { "light" },
            header {
                h1 { b { "rustd" } " playground" }
                p { "{shown.about}" }
                label {
                    "demo "
                    select {
                        onchange: move |event| {
                            if let Some(picked) = Demo::named(&event.value()) {
                                source.set(picked.source.to_owned());
                                demo.set(picked);
                            }
                        },
                        for it in DEMOS {
                            option { value: it.name, selected: it.name == shown.name, "{it.title}" }
                        }
                    }
                }
                button {
                    r#type: "button",
                    onclick: move |_| {
                        let now = !dark();
                        store_dark(now);
                        dark.set(now);
                    },
                    if dark() { "light mode" } else { "dark mode" }
                }
            }
            section { class: "pane",
                h2 { "src/main.rs" span { "compiled in this page on every edit" } }
                div { id: "editor",
                    // Another editor for another program: the text of one that was typed
                    // in is no longer what its `value` says. A key is only looked at in a list.
                    for name in [shown.name] {
                        CodeEditor {
                            key: "{name}",
                            value: source(),
                            language: Language::Rust,
                            theme: CodeTheme::fixed(theme),
                            line_numbers: true,
                            aria_label: "Rust source",
                            oninput: move |text: String| {
                                edit(&text);
                                source.set(text);
                            },
                        }
                    }
                }
            }
            // Both are always there: a program keeps what it has drawn into while another
            // is shown.
            section { class: "pane",
                h2 { if app { "app" } else { "canvas" } }
                canvas { id: "canvas", hidden: app }
                div { id: "main", hidden: !app }
            }
            footer {
                span { id: "state", class: "{kind}", "{kind}" }
                div { id: "status", class: "{kind}", "{text}" }
            }
        }
    }
}

/// What a program shows in.
#[derive(Clone, Copy, PartialEq)]
enum Output {
    /// It draws, with the functions `playground.js` gives it.
    Canvas,
    /// It is a Dioxus app, linked into the Dioxus rustc built.
    App,
}

/// A program of the picker.
#[derive(Clone, Copy, PartialEq)]
struct Demo {
    name: &'static str,
    title: &'static str,
    about: &'static str,
    source: &'static str,
    output: Output,
}

const CANVAS: &str = "A Rust compiler running as WebAssembly, emitting WebAssembly: edit the code and the changed functions are swapped into the running program.";

const DEMOS: &[Demo] = &[
    Demo {
        name: "balls",
        title: "bouncing balls",
        about: CANVAS,
        source: include_str!("../../demos/balls.rs"),
        output: Output::Canvas,
    },
    Demo {
        name: "life",
        title: "game of life",
        about: CANVAS,
        source: include_str!("../../demos/life.rs"),
        output: Output::Canvas,
    },
    Demo {
        name: "sort",
        title: "sorting, with trait objects",
        about: CANVAS,
        source: include_str!("../../demos/sort.rs"),
        output: Output::Canvas,
    },
    Demo {
        name: "mandelbrot",
        title: "mandelbrot set",
        about: CANVAS,
        source: include_str!("../../demos/mandelbrot.rs"),
        output: Output::Canvas,
    },
    Demo {
        name: "dioxus",
        title: "Dioxus counter",
        about: "A Dioxus app compiled in the page and linked into a Dioxus and std that rustc built: edit the rsx! and it re-renders with its state.",
        source: include_str!("../../dioxus/app/src/main.rs"),
        output: Output::App,
    },
];

impl Demo {
    fn named(name: &str) -> Option<Demo> {
        DEMOS.iter().copied().find(|it| it.name == name)
    }

    /// The one the address of the page names after its `#`, or the first.
    fn of_address() -> Demo {
        let hash = web_sys::window().and_then(|it| it.location().hash().ok()).unwrap_or_default();
        Demo::named(hash.trim_start_matches('#')).unwrap_or(DEMOS[0])
    }
}

fn set_address(name: &str) {
    if let Some(window) = web_sys::window() {
        _ = window.location().set_hash(name);
    }
}

const DARK_KEY: &str = "rustd-theme";

/// The theme picked before, or else the one of the system.
fn stored_dark() -> bool {
    let Some(window) = web_sys::window() else { return true };
    let stored = window.local_storage().ok().flatten().and_then(|it| it.get_item(DARK_KEY).ok());
    match stored.flatten().as_deref() {
        Some(name) => name != "light",
        None => !window
            .match_media("(prefers-color-scheme: light)")
            .ok()
            .flatten()
            .is_some_and(|it| it.matches()),
    }
}

fn store_dark(dark: bool) {
    let storage = web_sys::window().and_then(|it| it.local_storage().ok().flatten());
    if let Some(storage) = storage {
        _ = storage.set_item(DARK_KEY, if dark { "dark" } else { "light" });
    }
}

/// How the last build went: `kind` is `building`, `running` or `error`.
#[derive(Clone, PartialEq)]
struct Status {
    kind: String,
    text: String,
}

type Statuses = (UnboundedSender<Status>, Option<UnboundedReceiver<Status>>);

thread_local! {
    /// What `playground.js` said, for the app to take.
    static STATUSES: RefCell<Statuses> = RefCell::new({
        let (sender, receiver) = unbounded();
        (sender, Some(receiver))
    });
}

/// Called by `playground.js` with how a build went.
#[wasm_bindgen]
pub fn rustd_status(kind: String, text: String) {
    STATUSES.with_borrow(|(sender, _)| _ = sender.unbounded_send(Status { kind, text }));
}

#[wasm_bindgen]
extern "C" {
    /// Builds and runs another program, in place of the one shown.
    #[wasm_bindgen(js_namespace = ["window", "rustd"])]
    fn show(name: &str, source: &str);

    /// Builds the program shown again, from this source.
    #[wasm_bindgen(js_namespace = ["window", "rustd"])]
    fn edit(source: &str);
}
