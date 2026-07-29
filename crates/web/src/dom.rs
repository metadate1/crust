use wasm_bindgen::JsCast as _;
use wasm_bindgen::JsValue;
use web_sys::{
    Document, Element, HtmlButtonElement, HtmlCanvasElement, HtmlElement, HtmlInputElement,
    HtmlProgressElement, HtmlSelectElement, Window,
};

use crate::display::{DisplaySettings, OutputAspect, OutputRatio, RenderResolution};

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
    pub unlock_all: HtmlInputElement,
    pub smooth_motion: HtmlInputElement,
    pub extended_world: HtmlInputElement,
    pub camera_zoom: HtmlSelectElement,
    pub output_aspect: HtmlSelectElement,
    pub render_resolution: HtmlSelectElement,
    pub custom_render_width: HtmlInputElement,
    pub custom_render_height: HtmlInputElement,
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
    pub frame_rate: HtmlElement,
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
            unlock_all: by_id(&document, "unlockAll")?,
            smooth_motion: by_id(&document, "smoothMotion")?,
            extended_world: by_id(&document, "extendedWorld")?,
            camera_zoom: by_id(&document, "cameraZoom")?,
            output_aspect: by_id(&document, "outputAspect")?,
            render_resolution: by_id(&document, "renderResolution")?,
            custom_render_width: by_id(&document, "customRenderWidth")?,
            custom_render_height: by_id(&document, "customRenderHeight")?,
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
            frame_rate: by_id(&document, "frameRate")?,
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

    #[must_use]
    pub fn display_settings(&self) -> DisplaySettings {
        let projection_percent = self
            .camera_zoom
            .value()
            .parse::<u32>()
            .ok()
            .filter(|value| matches!(*value, 55 | 70 | 85 | 100))
            .unwrap_or(100);
        let aspect = OutputAspect::from_value(&self.output_aspect.value());
        let output_ratio = aspect
            .fixed_ratio()
            .unwrap_or_else(|| self.automatic_output_ratio());
        let extended_world = self.extended_world.checked()
            || projection_percent != 100
            || aspect != OutputAspect::Retail;
        if extended_world {
            self.extended_world.set_checked(true);
        }
        DisplaySettings {
            smooth_motion: self.smooth_motion.checked(),
            extended_world,
            projection_percent,
            aspect,
            output_ratio,
            resolution: RenderResolution::from_values(
                &self.render_resolution.value(),
                &self.custom_render_width.value(),
                &self.custom_render_height.value(),
            ),
        }
    }

    pub fn apply_display_settings(&self, settings: DisplaySettings) -> Result<(), JsValue> {
        self.screen
            .set_attribute("data-display-aspect", settings.aspect.value())?;
        self.screen.style().set_property(
            "--crust-output-aspect",
            &format!(
                "{} / {}",
                settings.output_ratio.numerator(),
                settings.output_ratio.denominator()
            ),
        )?;
        self.frame_rate
            .set_text_content(Some(if settings.smooth_motion {
                "30 Hz sim / refresh-smoothed display"
            } else {
                "30.00 Hz"
            }));
        Ok(())
    }

    pub fn sync_canvas_resolution(&self, settings: DisplaySettings) {
        let (width, height) = match settings.resolution {
            RenderResolution::Native => {
                let scale = window().map_or(1.0, |window| window.device_pixel_ratio());
                let width = (f64::from(self.canvas.client_width().max(1)) * scale).round();
                let height = (f64::from(self.canvas.client_height().max(1)) * scale).round();
                (safe_canvas_dimension(width), safe_canvas_dimension(height))
            }
            RenderResolution::Fixed(height) => {
                let width = settings.output_ratio.width_for_height(height);
                (width.clamp(1, 12_288), height.clamp(1, 12_288))
            }
            RenderResolution::Custom { width, height } => (width, height),
        };
        if self.canvas.width() != width {
            self.canvas.set_width(width);
        }
        if self.canvas.height() != height {
            self.canvas.set_height(height);
        }
    }

    fn automatic_output_ratio(&self) -> OutputRatio {
        let dimensions = window().ok().and_then(|window| {
            let width = window.inner_width().ok()?.as_f64()?;
            let height = window.inner_height().ok()?.as_f64()?;
            Some((
                safe_canvas_dimension(width.round()),
                safe_canvas_dimension(height.round()),
            ))
        });
        dimensions
            .and_then(|(width, height)| OutputRatio::from_dimensions(width, height))
            .unwrap_or(OutputRatio::WIDE)
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the finite value is clamped to the exact positive u32 subrange before conversion"
)]
fn safe_canvas_dimension(value: f64) -> u32 {
    if !value.is_finite() {
        return 1;
    }
    value.clamp(1.0, 12_288.0) as u32
}

#[allow(dead_code)]
fn _assert_element_traits(_: &Element, _: &HtmlProgressElement) {}
