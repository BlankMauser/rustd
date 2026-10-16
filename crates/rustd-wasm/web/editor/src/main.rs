//! The source pane: a highlighted editor the page puts text in with `rustd_editor_set`, and
//! hears the edits of through `window.rustdEdited`.

use std::cell::RefCell;

use dioxus::prelude::*;
use dioxus_code::{CodeTheme, Theme};
use dioxus_code_editor::{CodeEditor, Language};
use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures_util::StreamExt;
use wasm_bindgen::prelude::*;

/// What the page sets.
enum Set {
    Text(String),
    Dark(bool),
}

type Sets = (UnboundedSender<Set>, Option<UnboundedReceiver<Set>>);

thread_local! {
    /// What the page sets, which it can before the editor is there to take it.
    static FROM_PAGE: RefCell<Sets> = RefCell::new({
        let (sender, receiver) = unbounded();
        (sender, Some(receiver))
    });
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = window, js_name = rustdEdited)]
    fn edited(text: &str);
}

/// Replaces the text, as when the page shows another program.
#[wasm_bindgen]
pub fn rustd_editor_set(text: String) {
    set(Set::Text(text));
}

/// Has the colors be the ones of a dark page, or of a light one.
#[wasm_bindgen]
pub fn rustd_editor_dark(dark: bool) {
    set(Set::Dark(dark));
}

fn set(set: Set) {
    FROM_PAGE.with_borrow(|(sender, _)| _ = sender.unbounded_send(set));
}

fn main() {
    dioxus::LaunchBuilder::web()
        .with_cfg(dioxus::web::Config::new().rootname("editor"))
        .launch(App);
}

#[component]
fn App() -> Element {
    let mut source = use_signal(String::new);
    let mut dark = use_signal(|| true);
    use_future(move || async move {
        let Some(mut receiver) = FROM_PAGE.with_borrow_mut(|(_, receiver)| receiver.take()) else {
            return;
        };
        while let Some(set) = receiver.next().await {
            match set {
                Set::Text(text) => source.set(text),
                Set::Dark(is) => dark.set(is),
            }
        }
    });
    let theme = if dark() { Theme::TOKYO_NIGHT } else { Theme::GITHUB_LIGHT };
    rsx! {
        CodeEditor {
            value: source(),
            language: Language::Rust,
            theme: CodeTheme::fixed(theme),
            line_numbers: true,
            aria_label: "Rust source",
            oninput: move |text: String| {
                edited(&text);
                source.set(text);
            },
        }
    }
}
