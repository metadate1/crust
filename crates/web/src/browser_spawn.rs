//! Browser-host spawn failure classification shared by the Wasm runtime and
//! native unit tests.

use crust_audio::retail::RetailAudioError;
use crust_formats::binary::PageIndex;
use crust_sim::object_arena::SpawnError;
use crust_sim::paging::PagingError;
use crust_sim::retail_runtime::{NsfProgramError, RuntimeError, RuntimeSpawnAttempt};

#[derive(Debug)]
pub(crate) enum BrowserProgramError {
    Program(NsfProgramError),
    Audio(RetailAudioError),
    AudioAsset(String),
    Paging(PagingError),
    PagingPageMismatch {
        requested: PageIndex,
        resolved: PageIndex,
    },
}

impl std::fmt::Display for BrowserProgramError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Program(error) => write!(formatter, "stream program host: {error:?}"),
            Self::Audio(error) => write!(formatter, "retail audio engine: {error}"),
            Self::AudioAsset(error) => formatter.write_str(error),
            Self::Paging(error) => write!(formatter, "retail pager: {error:?}"),
            Self::PagingPageMismatch {
                requested,
                resolved,
            } => write!(
                formatter,
                "retail pager resolved page {}, but GOOL requested page {}",
                resolved.get(),
                requested.get(),
            ),
        }
    }
}

impl std::error::Error for BrowserProgramError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExpectedBrowserSpawnRejection {
    AlreadyActive,
    AuthoredInvalidStateSentinel,
}

fn expected_browser_spawn_rejection(
    error: &RuntimeError<BrowserProgramError>,
) -> Option<ExpectedBrowserSpawnRejection> {
    match error {
        RuntimeError::Spawn(
            SpawnError::SpawnBlocked { .. } | SpawnError::MainObjectAlreadyActive,
        ) => Some(ExpectedBrowserSpawnRejection::AlreadyActive),
        RuntimeError::Program(BrowserProgramError::Program(error))
            if error.is_invalid_state_sentinel_mapping() =>
        {
            Some(ExpectedBrowserSpawnRejection::AuthoredInvalidStateSentinel)
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct BrowserSpawnCounts {
    pub(crate) attempts: u64,
    pub(crate) successful: u64,
    pub(crate) already_active: u64,
    pub(crate) authored_rejections: u64,
    pub(crate) failed: u64,
}

impl BrowserSpawnCounts {
    fn record<T>(&mut self, result: &Result<T, RuntimeError<BrowserProgramError>>) {
        self.attempts = self.attempts.saturating_add(1);
        match result {
            Ok(_) => self.successful = self.successful.saturating_add(1),
            Err(error) => match expected_browser_spawn_rejection(error) {
                Some(ExpectedBrowserSpawnRejection::AlreadyActive) => {
                    self.already_active = self.already_active.saturating_add(1);
                }
                Some(ExpectedBrowserSpawnRejection::AuthoredInvalidStateSentinel) => {
                    self.authored_rejections = self.authored_rejections.saturating_add(1);
                }
                None => self.failed = self.failed.saturating_add(1),
            },
        }
    }
}

pub(crate) fn browser_spawn_counts(
    attempts: &[RuntimeSpawnAttempt<BrowserProgramError>],
) -> BrowserSpawnCounts {
    let mut counts = BrowserSpawnCounts::default();
    for attempt in attempts {
        counts.record(&attempt.result);
    }
    counts
}

pub(crate) fn first_unexpected_browser_spawn(
    attempts: &[RuntimeSpawnAttempt<BrowserProgramError>],
) -> Option<&RuntimeError<BrowserProgramError>> {
    attempts.iter().find_map(|attempt| {
        attempt
            .result
            .as_ref()
            .err()
            .filter(|error| expected_browser_spawn_rejection(error).is_none())
    })
}

#[cfg(test)]
mod tests {
    use crust_formats::binary::{Eid, FormatError};
    use crust_sim::object_arena::EntitySpawnDescriptor;

    use super::*;

    fn classify(error: RuntimeError<BrowserProgramError>) -> (BrowserSpawnCounts, bool) {
        let attempts = [RuntimeSpawnAttempt {
            neighbor_index: 0,
            entity_index: 0,
            zone: Eid::NONE,
            descriptor: EntitySpawnDescriptor {
                id: 0,
                group: 3,
                executable: 0,
                subtype: 0,
            },
            result: Err(error),
        }];
        (
            browser_spawn_counts(&attempts),
            first_unexpected_browser_spawn(&attempts).is_some(),
        )
    }

    #[test]
    fn authored_sentinel_is_the_only_wrapped_program_error_rejection() {
        let (sentinel, sentinel_is_unexpected) = classify(RuntimeError::Program(
            BrowserProgramError::Program(NsfProgramError::Format(FormatError::at(
                1_670_600,
                "GOOL subtype 4 maps to the invalid-state sentinel",
            ))),
        ));
        assert_eq!(
            sentinel,
            BrowserSpawnCounts {
                attempts: 1,
                authored_rejections: 1,
                ..BrowserSpawnCounts::default()
            }
        );
        assert!(!sentinel_is_unexpected);

        let (unrelated, unrelated_is_unexpected) =
            classify(RuntimeError::Program(BrowserProgramError::Program(
                NsfProgramError::Format(FormatError::global("unrelated malformed GOOL")),
            )));
        assert_eq!(
            unrelated,
            BrowserSpawnCounts {
                attempts: 1,
                failed: 1,
                ..BrowserSpawnCounts::default()
            }
        );
        assert!(unrelated_is_unexpected);
    }

    #[test]
    fn paging_audio_and_unexpected_spawn_errors_remain_failures() {
        for error in [
            RuntimeError::Program(BrowserProgramError::Paging(PagingError::TooManyPages)),
            RuntimeError::Program(BrowserProgramError::Audio(
                RetailAudioError::InvalidMaxMidiVoices(25),
            )),
            RuntimeError::Program(BrowserProgramError::AudioAsset(
                "missing authored audio asset".to_owned(),
            )),
            RuntimeError::Program(BrowserProgramError::PagingPageMismatch {
                requested: PageIndex::new(2),
                resolved: PageIndex::new(3),
            }),
            RuntimeError::Spawn(SpawnError::ObjectPoolFull),
        ] {
            let (counts, is_unexpected) = classify(error);
            assert_eq!(
                counts,
                BrowserSpawnCounts {
                    attempts: 1,
                    failed: 1,
                    ..BrowserSpawnCounts::default()
                }
            );
            assert!(is_unexpected);
        }
    }
}
