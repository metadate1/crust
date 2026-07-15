//! Browser adapter for native's pause-adjusted 32-bit wall clock.

pub(crate) fn advance_retail_shader_clock(
    elapsed: &mut u32,
    previous_wall_ticks: &mut Option<u32>,
    wall_ticks: u32,
    was_paused: bool,
) {
    if let Some(previous) = *previous_wall_ticks
        && !was_paused
    {
        *elapsed = elapsed.wrapping_add(wall_ticks.wrapping_sub(previous));
    }
    *previous_wall_ticks = Some(wall_ticks);
}

#[cfg(test)]
mod tests {
    use super::advance_retail_shader_clock;

    #[test]
    fn clock_includes_cooperative_gaps_but_excludes_the_complete_pause_interval() {
        let mut elapsed = 0_u32;
        let mut previous = None;
        advance_retail_shader_clock(&mut elapsed, &mut previous, 1_000, false);
        advance_retail_shader_clock(&mut elapsed, &mut previous, 1_034, false);
        // The pause-opening frame reaches its native pause stamp.
        advance_retail_shader_clock(&mut elapsed, &mut previous, 1_068, false);
        // Paused callbacks and the resume callback do not advance it.
        advance_retail_shader_clock(&mut elapsed, &mut previous, 5_000, true);
        advance_retail_shader_clock(&mut elapsed, &mut previous, 9_000, true);
        assert_eq!(elapsed, 68);
        // One late cooperative callback includes the complete unpaused gap.
        advance_retail_shader_clock(&mut elapsed, &mut previous, 9_500, false);
        assert_eq!(elapsed, 568);
    }

    #[test]
    fn clock_delta_uses_native_wrapping_words() {
        let mut elapsed = 10_u32;
        let mut previous = Some(u32::MAX - 3);
        advance_retail_shader_clock(&mut elapsed, &mut previous, 2, false);
        assert_eq!(elapsed, 16);
    }

    #[test]
    fn clock_includes_an_asynchronous_mount_gap() {
        let mut elapsed = 70_u32;
        let mut previous = Some(1_000_u32);
        // The caller marks asset stalls as unpaused because native's
        // synchronous NSKill/NSInit never stops GetTicksElapsed.
        advance_retail_shader_clock(&mut elapsed, &mut previous, 5_096, false);
        assert_eq!(elapsed, 4_166);
    }
}
