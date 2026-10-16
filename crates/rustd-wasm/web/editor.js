// The source pane. It is `dioxus-code-editor`, a module rustc built (`build-editor.sh`), and
// a plain text area where that is not there.

export class Editor {
  // `edited` is called with the text after every edit made in the pane.
  static async load(element, edited) {
    const editor = new Editor();
    editor.text = "";
    window.rustdEdited = (text) => {
      editor.text = text;
      edited(text);
    };
    try {
      element.id = "editor";
      // The module starts itself, into the element of that name.
      const { rustd_editor_set, rustd_editor_dark } = await import("./assets/editor.js");
      while (!globalThis.__dx_mainWasm) {
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      editor.set = rustd_editor_set;
      const themed = () => rustd_editor_dark(document.documentElement.dataset.theme !== "light");
      window.addEventListener("rustd-theme", themed);
      themed();
    } catch (error) {
      console.warn("no highlighting:", error);
      const area = element.appendChild(document.createElement("textarea"));
      area.spellcheck = false;
      area.addEventListener("input", () => window.rustdEdited(area.value));
      editor.set = (text) => (area.value = text);
    }
    // Tab is four spaces, in either kind of pane.
    element.addEventListener("keydown", (event) => {
      if (event.key === "Tab" && event.target instanceof HTMLTextAreaElement) {
        event.preventDefault();
        const area = event.target;
        area.setRangeText("    ", area.selectionStart, area.selectionEnd, "end");
        area.dispatchEvent(new InputEvent("input", { bubbles: true }));
      }
    });
    return editor;
  }

  get value() {
    return this.text;
  }

  // Shows another text, which is not an edit.
  set value(text) {
    this.text = text;
    this.set(text);
  }
}
