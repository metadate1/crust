//! Keyboard, standard gamepad, touch, and demo pad mapping.

pub const PAD_L2: u16 = 0x0001;
pub const PAD_R2: u16 = 0x0002;
pub const PAD_L1: u16 = 0x0004;
pub const PAD_R1: u16 = 0x0008;
pub const PAD_TRIANGLE: u16 = 0x0010;
pub const PAD_CIRCLE: u16 = 0x0020;
pub const PAD_CROSS: u16 = 0x0040;
pub const PAD_SQUARE: u16 = 0x0080;
pub const PAD_SELECT: u16 = 0x0100;
pub const PAD_L3: u16 = 0x0200;
pub const PAD_R3: u16 = 0x0400;
pub const PAD_START: u16 = 0x0800;
pub const PAD_UP: u16 = 0x1000;
pub const PAD_RIGHT: u16 = 0x2000;
pub const PAD_DOWN: u16 = 0x4000;
pub const PAD_LEFT: u16 = 0x8000;
pub const TAP_MASK: u16 = 0xf9ff;
pub const AXIS_DEADZONE: f32 = 16_000.0 / 32_767.0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PadSnapshot {
    pub held: u16,
    pub tapped: u16,
    pub held_previous: u16,
    pub held_previous_2: u16,
    pub tapped_previous: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PadState {
    snapshot: PadSnapshot,
}

impl PadState {
    #[must_use]
    pub const fn snapshot(&self) -> PadSnapshot {
        self.snapshot
    }

    pub fn update(&mut self, physical: u16, touch: u16, demo_override: Option<u16>) {
        let mut held = demo_override.unwrap_or(physical | touch);
        // The console resolves impossible opposing inputs in a stable direction.
        if held & PAD_UP != 0 {
            held &= !PAD_DOWN;
        }
        if held & PAD_LEFT != 0 {
            held &= !PAD_RIGHT;
        }
        let previous = self.snapshot.held;
        self.snapshot.held_previous_2 = self.snapshot.held_previous;
        self.snapshot.tapped_previous = self.snapshot.tapped;
        self.snapshot.held_previous = previous;
        self.snapshot.held = held;
        self.snapshot.tapped = (!previous & held) & TAP_MASK;
    }

    pub fn clear(&mut self) {
        self.update(0, 0, None);
    }
}

/// Map a browser `KeyboardEvent.code` value. Using codes keeps controls tied to physical keys
/// across keyboard layouts.
#[must_use]
pub fn keyboard_code(code: &str) -> Option<u16> {
    Some(match code {
        "Space" => PAD_SELECT,
        "KeyK" => PAD_L3,
        "KeyL" => PAD_R3,
        "Enter" | "NumpadEnter" => PAD_START,
        "ArrowUp" => PAD_UP,
        "ArrowRight" => PAD_RIGHT,
        "ArrowDown" => PAD_DOWN,
        "ArrowLeft" => PAD_LEFT,
        "KeyQ" => PAD_L2,
        "KeyW" => PAD_R2,
        "KeyA" => PAD_L1,
        "KeyS" => PAD_R1,
        "KeyV" => PAD_TRIANGLE,
        "KeyC" => PAD_CIRCLE,
        "KeyZ" => PAD_CROSS,
        "KeyX" => PAD_SQUARE,
        _ => return None,
    })
}

/// Map the standard Gamepad API layout and digitalize the left stick.
#[must_use]
pub fn standard_gamepad(buttons: &[bool], axes: &[f32]) -> u16 {
    const BUTTON_BITS: [u16; 16] = [
        PAD_CROSS,
        PAD_CIRCLE,
        PAD_SQUARE,
        PAD_TRIANGLE,
        PAD_L1,
        PAD_R1,
        PAD_L2,
        PAD_R2,
        PAD_SELECT,
        PAD_START,
        PAD_L3,
        PAD_R3,
        PAD_UP,
        PAD_DOWN,
        PAD_LEFT,
        PAD_RIGHT,
    ];
    let mut held = 0_u16;
    for (pressed, bit) in buttons.iter().copied().zip(BUTTON_BITS) {
        if pressed {
            held |= bit;
        }
    }
    // The Gamepad specification requires finite normalized values. Treat a
    // broken device/browser value as centered instead of synthesizing input.
    let axis_x = gamepad_axis(axes.first().copied());
    let axis_y = gamepad_axis(axes.get(1).copied());
    if axis_x < -AXIS_DEADZONE {
        held |= PAD_LEFT;
    }
    if axis_x > AXIS_DEADZONE {
        held |= PAD_RIGHT;
    }
    if axis_y < -AXIS_DEADZONE {
        held |= PAD_UP;
    }
    if axis_y > AXIS_DEADZONE {
        held |= PAD_DOWN;
    }
    held
}

fn gamepad_axis(value: Option<f32>) -> f32 {
    value.filter(|axis| axis.is_finite()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_complete_keyboard_pad() {
        let codes = [
            "Space",
            "KeyK",
            "KeyL",
            "Enter",
            "ArrowUp",
            "ArrowRight",
            "ArrowDown",
            "ArrowLeft",
            "KeyQ",
            "KeyW",
            "KeyA",
            "KeyS",
            "KeyV",
            "KeyC",
            "KeyZ",
            "KeyX",
        ];
        let mapped = codes
            .into_iter()
            .map(|code| keyboard_code(code).unwrap())
            .fold(0_u16, |value, bit| value | bit);
        assert_eq!(mapped, u16::MAX);
    }

    #[test]
    fn mapped_gamepad_matches_standard_order() {
        for index in 0..16 {
            let mut buttons = [false; 16];
            buttons[index] = true;
            assert_eq!(standard_gamepad(&buttons, &[]).count_ones(), 1);
        }
    }

    #[test]
    fn analog_deadzone_is_strict() {
        assert_eq!(standard_gamepad(&[], &[AXIS_DEADZONE, 0.0]), 0);
        assert_eq!(
            standard_gamepad(&[], &[AXIS_DEADZONE + 0.001, 0.0]),
            PAD_RIGHT
        );
        assert_eq!(
            standard_gamepad(&[], &[-AXIS_DEADZONE - 0.001, 0.0]),
            PAD_LEFT
        );
    }

    #[test]
    fn non_finite_gamepad_axes_are_centered() {
        assert_eq!(standard_gamepad(&[], &[f32::NAN, f32::INFINITY]), 0);
        assert_eq!(standard_gamepad(&[], &[f32::NEG_INFINITY, f32::NAN]), 0);
    }

    #[test]
    fn up_and_left_win_opposing_inputs() {
        let mut pad = PadState::default();
        pad.update(PAD_UP | PAD_DOWN | PAD_LEFT | PAD_RIGHT, 0, None);
        assert_eq!(pad.snapshot().held, PAD_UP | PAD_LEFT);
    }

    #[test]
    fn tap_edges_exclude_stick_clicks() {
        let mut pad = PadState::default();
        pad.update(PAD_CROSS | PAD_L3 | PAD_R3, 0, None);
        assert_eq!(pad.snapshot().tapped, PAD_CROSS);
        pad.update(PAD_CROSS, 0, None);
        assert_eq!(pad.snapshot().tapped, 0);
        pad.clear();
        pad.update(PAD_CROSS, 0, None);
        assert_eq!(pad.snapshot().tapped, PAD_CROSS);
    }

    #[test]
    fn demo_replaces_live_sources() {
        let mut pad = PadState::default();
        pad.update(PAD_CROSS, PAD_SQUARE, Some(PAD_START));
        assert_eq!(pad.snapshot().held, PAD_START);
    }
}
