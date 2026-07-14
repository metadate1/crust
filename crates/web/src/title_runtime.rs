//! Pointer-free policy for the retail title screen runtime.
//!
//! The source mutates global display flags and LDAT FOV for three distinct
//! title screen types. The browser keeps parsed game data immutable, so these
//! small values are selected explicitly and passed to simulation/render hosts.

use crust_formats::{binary::Eid, stream::ZoneEntity};
use crust_sim::flow::{TitlePhase, TitleScreen};

pub(crate) const RETAIL_ZONE_OBJECTS_ACTIVE: u32 = 0x0002;
pub(crate) const RETAIL_CAMERA_UPDATE: u32 = 0x0002;

// `GLDrawOverlay` does not use the fade counter as linear alpha. It selects
// one of these sixteen source alpha levels after quantizing brightness to a
// four-bit band. Keeping the table here avoids approximating the title-card
// presentation with browser/CSS opacity.
const RETAIL_FADE_ALPHA: [u8; 16] = [
    12, 25, 38, 51, 64, 77, 91, 105, 120, 135, 151, 167, 185, 203, 225, 255,
];

const fn clamp_retail_brightness(brightness: i32) -> i32 {
    if brightness < 0 {
        0
    } else if brightness > 256 {
        256
    } else {
        brightness
    }
}

const fn retail_overlay_alpha(brightness: i32) -> u8 {
    if brightness == 0 {
        return 0;
    }
    let shifted = brightness.cast_unsigned() >> 4;
    let band = if shifted < 1 {
        0
    } else if shifted > 16 {
        15
    } else {
        shifted - 1
    };
    RETAIL_FADE_ALPHA[band as usize]
}

/// Returns the black-overlay alpha drawn by native `GLUpdate` for the live
/// retail title presentation.
///
/// The counter is authoritative global 106 after the native GL fade step.
/// Fading in uses that value directly; fading out converts its signed
/// `-256..=0` representation to brightness. Native `TitleUpdate` also draws
/// opaque black while swapping title states.
#[must_use]
pub(crate) const fn retail_title_overlay_alpha(
    phase: TitlePhase,
    fade_counter: i32,
    opaque_swap_overlay: bool,
) -> u8 {
    if opaque_swap_overlay {
        return u8::MAX;
    }
    match phase {
        TitlePhase::Start | TitlePhase::Blank | TitlePhase::FinishedFadingOut => u8::MAX,
        TitlePhase::FadingIn => retail_overlay_alpha(clamp_retail_brightness(fade_counter)),
        TitlePhase::FadingOut => {
            retail_overlay_alpha(clamp_retail_brightness(fade_counter.saturating_add(256)))
        }
        TitlePhase::Ready => 0,
    }
}

/// Separates the type-17 entry that owns a title entity from the ZDAT stored
/// in the spawned object's `obj_zone` field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetailTitleMdatBinding {
    pub(crate) source: Eid,
    pub(crate) object_zone: Eid,
}

/// Mirrors `GoolObjectSpawn`'s type-17 rewrite to native `cur_zone`.
#[must_use]
pub(crate) const fn retail_title_mdat_binding(
    source: Eid,
    current_zone: Eid,
) -> RetailTitleMdatBinding {
    RetailTitleMdatBinding {
        source,
        object_zone: current_zone,
    }
}

const RETAIL_DISPLAY_WORLDS: u32 = 0x0001;
const RETAIL_PRESERVED_OBJECT_CATEGORIES: u32 = 0x3ff0;
const RETAIL_DISPLAY_ANIMATE_OBJECTS: u32 = 0xfffc;
const RETAIL_DISPLAY_IMAGES: u32 = 0x2_0000;
const RETAIL_TITLE_LOADED: u32 = 0x20_0000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RetailTitleScreenType {
    ImageOnly,
    WorldObjectsAndCamera,
    ImageAndObjects,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetailTitleScreenProfile {
    pub(crate) screen_type: RetailTitleScreenType,
    pub(crate) zone_name: &'static str,
    pub(crate) field_of_view: u32,
}

impl RetailTitleScreenProfile {
    pub(crate) const fn uses_image(self) -> bool {
        matches!(
            self.screen_type,
            RetailTitleScreenType::ImageOnly | RetailTitleScreenType::ImageAndObjects
        )
    }

    pub(crate) const fn updates_camera(self) -> bool {
        matches!(
            self.screen_type,
            RetailTitleScreenType::WorldObjectsAndCamera
        )
    }

    pub(crate) const fn display_mask(self) -> u32 {
        let type_mask = match self.screen_type {
            // Native type-zero TitleLoadScreen clears only DISPANIM_ALL,
            // deliberately retaining the per-category object bits while
            // clearing the global DISPLAY | ANIMATE pair. The immediately
            // following TitleUpdate start/blank branch restores that pair
            // before GLUpdate latches the word. Retaining the 0x3ff0 category
            // tail is what lets the otherwise invisible MDAT controller run
            // on the following publisher-card frame.
            RetailTitleScreenType::ImageOnly => {
                RETAIL_DISPLAY_IMAGES | RETAIL_PRESERVED_OBJECT_CATEGORIES
            }
            RetailTitleScreenType::WorldObjectsAndCamera => {
                RETAIL_DISPLAY_WORLDS | RETAIL_DISPLAY_ANIMATE_OBJECTS | RETAIL_CAMERA_UPDATE
            }
            RetailTitleScreenType::ImageAndObjects => {
                RETAIL_DISPLAY_ANIMATE_OBJECTS | RETAIL_DISPLAY_IMAGES
            }
        };
        type_mask | RETAIL_TITLE_LOADED
    }
}

pub(crate) const fn retail_title_screen_profile(
    screen: TitleScreen,
    current_map_level: u32,
) -> RetailTitleScreenProfile {
    match screen {
        TitleScreen::MainMenu => RetailTitleScreenProfile {
            screen_type: RetailTitleScreenType::ImageAndObjects,
            zone_name: "0c_pZ",
            field_of_view: 55,
        },
        TitleScreen::Options => RetailTitleScreenProfile {
            screen_type: RetailTitleScreenType::WorldObjectsAndCamera,
            zone_name: "0f_pZ",
            field_of_view: 37,
        },
        TitleScreen::PublisherFirst | TitleScreen::PublisherSecond => RetailTitleScreenProfile {
            screen_type: RetailTitleScreenType::ImageOnly,
            zone_name: "0a_pZ",
            field_of_view: 90,
        },
        TitleScreen::NaughtyDog => RetailTitleScreenProfile {
            screen_type: RetailTitleScreenType::ImageAndObjects,
            zone_name: "0d_pZ",
            field_of_view: 55,
        },
        TitleScreen::GameOver => RetailTitleScreenProfile {
            screen_type: RetailTitleScreenType::WorldObjectsAndCamera,
            zone_name: "0b_pZ",
            field_of_view: 55,
        },
        TitleScreen::Password | TitleScreen::Load => RetailTitleScreenProfile {
            screen_type: RetailTitleScreenType::WorldObjectsAndCamera,
            zone_name: "0e_pZ",
            field_of_view: 55,
        },
        TitleScreen::Map => RetailTitleScreenProfile {
            screen_type: RetailTitleScreenType::WorldObjectsAndCamera,
            zone_name: if current_map_level == 99 || current_map_level < 9 {
                "1a_pZ"
            } else if current_map_level == 9 {
                "1e_pZ"
            } else if current_map_level < 18 {
                "2b_pZ"
            } else {
                "3a_pZ"
            },
            field_of_view: 37,
        },
    }
}

pub(crate) const fn title_state_number_uses_image(state: u8) -> bool {
    matches!(state, 5 | 7 | 8 | 10)
}

pub(crate) fn title_mdat_entity_is_unlocked(entity: &ZoneEntity, levels_unlocked: u32) -> bool {
    entity
        .path_points
        .first()
        .is_some_and(|point| i64::from(point.z) <= i64::from(levels_unlocked))
}

#[cfg(test)]
mod tests {
    use crust_formats::binary::EntryRef;
    use crust_formats::stream::ZoneEntityPathPoint;

    use super::*;

    fn title_entity(z: i16) -> ZoneEntity {
        ZoneEntity {
            serialized_parent: EntryRef::from_raw(0),
            spawn_flags: 0,
            group: 3,
            id: 0,
            initializer: [0; 3],
            executable: 0,
            subtype: 0,
            path_points: vec![ZoneEntityPathPoint { x: 0, y: 0, z }],
        }
    }

    #[test]
    fn profiles_preserve_source_types_fov_display_masks_and_map_zones() {
        let image_only = retail_title_screen_profile(TitleScreen::PublisherFirst, 0);
        assert_eq!(image_only.screen_type, RetailTitleScreenType::ImageOnly);
        assert_eq!(image_only.zone_name, "0a_pZ");
        assert_eq!(image_only.field_of_view, 90);
        assert_eq!(image_only.display_mask(), 0x22_3ff0);
        assert!(image_only.uses_image());
        assert!(!image_only.updates_camera());

        let image_objects = retail_title_screen_profile(TitleScreen::MainMenu, 0);
        assert_eq!(
            image_objects.screen_type,
            RetailTitleScreenType::ImageAndObjects
        );
        assert_eq!(image_objects.field_of_view, 55);
        assert_eq!(image_objects.display_mask(), 0x22_fffc);

        let world = retail_title_screen_profile(TitleScreen::Options, 0);
        assert_eq!(
            world.screen_type,
            RetailTitleScreenType::WorldObjectsAndCamera
        );
        assert_eq!(world.field_of_view, 37);
        assert_eq!(world.display_mask(), 0x20_ffff);
        assert!(!world.uses_image());
        assert!(world.updates_camera());
        assert_eq!(RETAIL_ZONE_OBJECTS_ACTIVE, 2);
        assert_eq!(
            retail_title_screen_profile(TitleScreen::Map, 8).zone_name,
            "1a_pZ"
        );
        assert_eq!(
            retail_title_screen_profile(TitleScreen::Map, 9).zone_name,
            "1e_pZ"
        );
        assert_eq!(
            retail_title_screen_profile(TitleScreen::Map, 17).zone_name,
            "2b_pZ"
        );
        assert_eq!(
            retail_title_screen_profile(TitleScreen::Map, 18).zone_name,
            "3a_pZ"
        );
    }

    #[test]
    fn title_overlay_matches_native_post_step_alpha_curve() {
        let fade_in_counters = [256, 224, 192, 160, 128, 96, 64, 32, 0];
        let fade_in_alpha = fade_in_counters
            .map(|counter| retail_title_overlay_alpha(TitlePhase::FadingIn, counter, false));
        assert_eq!(fade_in_alpha, [u8::MAX, 203, 167, 135, 105, 77, 51, 25, 0]);

        let fade_out_counters = [-224, -192, -160, -128, -96, -64, -32, 0];
        let fade_out_alpha = fade_out_counters
            .map(|counter| retail_title_overlay_alpha(TitlePhase::FadingOut, counter, false));
        assert_eq!(fade_out_alpha, [25, 51, 77, 105, 135, 167, 203, u8::MAX]);
    }

    #[test]
    fn title_overlay_preserves_immediate_native_draw_across_phase_retargets() {
        for phase in [
            TitlePhase::Start,
            TitlePhase::Blank,
            TitlePhase::FinishedFadingOut,
        ] {
            assert_eq!(retail_title_overlay_alpha(phase, 0, false), u8::MAX);
        }
        assert_eq!(
            retail_title_overlay_alpha(TitlePhase::FadingOut, -224, true),
            u8::MAX,
            "an exact-zero or TitleLoadState overlay survives a same-frame fade-out retarget"
        );
        assert_eq!(
            retail_title_overlay_alpha(TitlePhase::FadingOut, -224, false),
            25,
            "the following source frame resumes the ordinary fade curve"
        );
        assert_eq!(retail_title_overlay_alpha(TitlePhase::Ready, 288, false), 0);

        // Malformed counters cannot wrap into an invalid alpha-table index.
        assert_eq!(
            retail_title_overlay_alpha(TitlePhase::FadingIn, i32::MAX, false),
            u8::MAX
        );
        assert_eq!(
            retail_title_overlay_alpha(TitlePhase::FadingOut, i32::MIN, false),
            0
        );
    }

    #[test]
    fn unlock_filter_uses_the_first_signed_path_z() {
        assert!(title_mdat_entity_is_unlocked(&title_entity(-1), 0));
        assert!(title_mdat_entity_is_unlocked(&title_entity(3), 3));
        assert!(!title_mdat_entity_is_unlocked(&title_entity(4), 3));

        let mut later_point = title_entity(4);
        later_point
            .path_points
            .push(ZoneEntityPathPoint { x: 0, y: 0, z: 0 });
        assert!(!title_mdat_entity_is_unlocked(&later_point, 3));
    }

    #[test]
    fn image_states_match_the_authored_mdat_states() {
        assert!(title_state_number_uses_image(5));
        assert!(title_state_number_uses_image(7));
        assert!(title_state_number_uses_image(8));
        assert!(title_state_number_uses_image(10));
        assert!(!title_state_number_uses_image(6));
        assert!(!title_state_number_uses_image(15));
    }

    #[test]
    fn mdat_entity_provenance_is_distinct_from_native_current_object_zone() {
        let source = Eid::from_raw(0x1111_1111);
        let current_zone = Eid::from_raw(0x2222_2222);

        assert_eq!(
            retail_title_mdat_binding(source, current_zone),
            RetailTitleMdatBinding {
                source,
                object_zone: current_zone,
            }
        );
    }
}
