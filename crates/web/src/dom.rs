use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use web_sys::{
    Document, Element, HtmlButtonElement, HtmlCanvasElement, HtmlElement, HtmlInputElement,
    HtmlProgressElement, HtmlSelectElement, Window,
};

pub fn window() -> Result<Window, JsValue> {
    web_sys::window().ok_or_else(|| JsValue::from_str("browser window is unavailable"))
}

pub fn document() -> Result<Document, JsValue> {
    window()?
        .document()
        .ok_or_else(|| JsValue::from_str("browser document is unavailable"))
}

fn by_id<T: wasm_bindgen::JsCast>(document: &Document, id: &str) -> Result<T, JsValue> {
    document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing #{id}")))?
        .dyn_into::<T>()
        .map_err(|_| JsValue::from_str(&format!("#{id} has the wrong element type")))
}

#[derive(Clone, Debug)]
pub struct Dom {
    pub document: Document,
    pub shell: HtmlElement,
    pub canvas: HtmlCanvasElement,
    pub screen: HtmlElement,
    pub status: HtmlElement,
    pub dropzone: HtmlElement,
    pub game_files: HtmlInputElement,
    pub game_folder: HtmlInputElement,
    pub choose_files: HtmlButtonElement,
    pub choose_folder: HtmlButtonElement,
    pub file_count: HtmlElement,
    pub pair_count: HtmlElement,
    pub byte_count: HtmlElement,
    pub asset_message: HtmlElement,
    pub boot_level: HtmlSelectElement,
    pub launch: HtmlButtonElement,
    pub clear: HtmlButtonElement,
    pub import_progress: HtmlElement,
    pub progress: HtmlElement,
    pub progress_label: HtmlElement,
    pub progress_value: HtmlElement,
    pub pause: HtmlButtonElement,
    pub mute: HtmlButtonElement,
    pub fullscreen: HtmlButtonElement,
    pub sim_state: HtmlElement,
    pub current_level: HtmlElement,
    pub audio_state: HtmlElement,
    pub card_state: HtmlElement,
    pub runtime_log: HtmlElement,
    pub game_overlay: HtmlElement,
    pub game_overline: HtmlElement,
    pub game_title: HtmlElement,
    pub game_subtitle: HtmlElement,
    pub game_menu: HtmlElement,
}

impl Dom {
    pub fn find() -> Result<Self, JsValue> {
        let document = document()?;
        Ok(Self {
            shell: document
                .query_selector(".shell")?
                .ok_or_else(|| JsValue::from_str("missing .shell"))?
                .dyn_into()?,
            canvas: by_id(&document, "canvas")?,
            screen: by_id(&document, "screen")?,
            status: by_id(&document, "runtimeStatus")?,
            dropzone: by_id(&document, "dropzone")?,
            game_files: by_id(&document, "gameFiles")?,
            game_folder: by_id(&document, "gameFolder")?,
            choose_files: by_id(&document, "chooseFiles")?,
            choose_folder: by_id(&document, "chooseFolder")?,
            file_count: by_id(&document, "fileCount")?,
            pair_count: by_id(&document, "pairCount")?,
            byte_count: by_id(&document, "byteCount")?,
            asset_message: by_id(&document, "assetMessage")?,
            boot_level: by_id(&document, "bootLevel")?,
            launch: by_id(&document, "launch")?,
            clear: by_id(&document, "clearData")?,
            import_progress: by_id(&document, "importProgress")?,
            progress: by_id(&document, "progressBar")?,
            progress_label: by_id(&document, "progressLabel")?,
            progress_value: by_id(&document, "progressValue")?,
            pause: by_id(&document, "pause")?,
            mute: by_id(&document, "mute")?,
            fullscreen: by_id(&document, "fullscreen")?,
            sim_state: by_id(&document, "simState")?,
            current_level: by_id(&document, "currentLevel")?,
            audio_state: by_id(&document, "audioState")?,
            card_state: by_id(&document, "cardState")?,
            runtime_log: by_id(&document, "runtimeLog")?,
            game_overlay: by_id(&document, "gameOverlay")?,
            game_overline: by_id(&document, "gameOverline")?,
            game_title: by_id(&document, "gameTitle")?,
            game_subtitle: by_id(&document, "gameSubtitle")?,
            game_menu: by_id(&document, "gameMenu")?,
            document,
        })
    }

    pub fn set_runtime_state(&self, state: &str, status: &str) -> Result<(), JsValue> {
        self.shell.set_attribute("data-runtime-state", state)?;
        self.status.set_text_content(Some(status));
        Ok(())
    }

    pub fn set_progress(&self, visible: bool, fraction: f64, label: &str) -> Result<(), JsValue> {
        self.import_progress.set_hidden(!visible);
        let percent = (fraction.clamp(0.0, 1.0) * 100.0).round();
        self.progress
            .style()
            .set_property("width", &format!("{percent:.0}%"))?;
        self.progress_label.set_text_content(Some(label));
        self.progress_value
            .set_text_content(Some(&format!("{percent:.0}%")));
        Ok(())
    }

    pub fn log(&self, message: &str, error: bool) {
        let old = self.runtime_log.text_content().unwrap_or_default();
        let marker = if error { '!' } else { '>' };
        let mut lines: Vec<_> = old.lines().collect();
        if lines.len() > 80 {
            lines.drain(0..lines.len() - 80);
        }
        let mut output = lines.join("\n");
        output.push('\n');
        output.push(marker);
        output.push(' ');
        output.push_str(message);
        self.runtime_log.set_text_content(Some(&output));
        self.runtime_log
            .set_scroll_top(self.runtime_log.scroll_height());
    }

    pub fn set_menu(&self, entries: &[(&str, bool)]) -> Result<(), JsValue> {
        self.game_menu.set_text_content(None);
        for (label, selected) in entries {
            let item = self.document.create_element("li")?;
            item.set_text_content(Some(label));
            if *selected {
                item.set_class_name("is-selected");
            }
            self.game_menu.append_child(&item)?;
        }
        Ok(())
    }

    pub fn set_overlay(&self, visible: bool, overline: &str, title: &str, subtitle: &str) {
        self.game_overlay.set_hidden(!visible);
        self.game_overline.set_text_content(Some(overline));
        self.game_title.set_text_content(Some(title));
        self.game_subtitle.set_text_content(Some(subtitle));
    }

    pub fn enable_runtime_controls(&self, enabled: bool) {
        self.pause.set_disabled(!enabled);
        self.mute.set_disabled(!enabled);
        let fullscreen_capable = self.document.fullscreen_enabled();
        self.fullscreen
            .set_disabled(!enabled || !fullscreen_capable);
        let fullscreen_hint = if !fullscreen_capable {
            "Fullscreen is unavailable in this browser context"
        } else if enabled {
            "Enter fullscreen"
        } else {
            "Fullscreen is available after launch"
        };
        let _ = self.fullscreen.set_attribute("title", fullscreen_hint);
    }
}

#[allow(dead_code)]
fn _assert_element_traits(_: &Element, _: &HtmlProgressElement) {}
