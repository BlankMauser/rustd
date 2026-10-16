//! Dioxus and `std`, built by rustc for the page to load before the program rustd compiles.

use std::{cell::RefCell, rc::Rc};

pub use dioxus;
use dioxus::dioxus_core::{Element, Runtime, VirtualDom};
use wasm_bindgen::prelude::wasm_bindgen;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn error(text: &str);
}

thread_local! {
    static RUNTIME: RefCell<Option<Rc<Runtime>>> = const { RefCell::new(None) };
}

/// Mounts the component, which is a function of the program in the table this module shares
/// with it.
#[unsafe(no_mangle)]
pub extern "C" fn rustd_launch(app: fn() -> Element) {
    std::panic::set_hook(Box::new(|info| error(&info.to_string())));
    let dom = VirtualDom::new(app);
    RUNTIME.set(Some(dom.runtime()));
    dioxus::web::launch::launch_virtual_dom(dom, dioxus::web::Config::new());
}

/// Runs every component again, for the page to call once it has swapped functions.
#[unsafe(no_mangle)]
pub extern "C" fn rustd_rerender() {
    RUNTIME.with_borrow(|runtime| {
        if let Some(runtime) = runtime {
            runtime.force_all_dirty();
        }
    });
}
