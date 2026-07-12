//! Bounded decoded-texture cache with frame-stable page generations.

use core::fmt;
use std::collections::HashMap;
use std::sync::Arc;

use crate::command::BlendMode;
use crate::texture::{
    ClutLocation, ColorMode, DecodedTexture, Palette, TEXTURE_PAGE_BYTES, TextureError,
    TextureRegion,
};

/// C1's fallback decoded-texture budget.
pub const DEFAULT_TEXTURE_BUDGET_BYTES: usize = 32 * 1024 * 1024;
/// Maximum number of separately owned decoded textures in the legacy cache.
pub const DEFAULT_TEXTURE_ENTRY_LIMIT: usize = 2048;
/// Active texture-page slots corresponding to PSX VRAM pages 8 through 15.
pub const TEXTURE_PAGE_SLOT_COUNT: usize = 8;

/// Stable renderer-side texture identifier. It contains no native pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureHandle(u64);

impl TextureHandle {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Complete key needed to decode a texture image from a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureRequest {
    pub page_id: u32,
    pub region: TextureRegion,
    pub color_mode: ColorMode,
    pub blend_mode: BlendMode,
    /// Required for indexed modes and ignored by direct-color mode.
    pub clut: Option<ClutLocation>,
}

/// Pixel-center UV limits inside the cache's one-pixel duplicated border.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextureUvBounds {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// A cache lease can safely outlive later cache eviction because pixel storage
/// is reference counted.
#[derive(Debug, Clone)]
pub struct CachedTexture {
    pub handle: TextureHandle,
    pub page_id: u32,
    pub page_generation: u64,
    pub pixels: Arc<DecodedTexture>,
    pub content_uv: TextureUvBounds,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheCounters {
    pub requests: u64,
    pub hits: u64,
    pub misses: u64,
    pub failures: u64,
    pub missing_pages: u64,
    pub generation_misses: u64,
    pub cache_failures: u64,
    pub page_changes: u64,
    pub generation_invalidations: u64,
    pub evictions: u64,
    pub upload_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheMetrics {
    pub frame: CacheCounters,
    pub total: CacheCounters,
    pub resident_entries: usize,
    pub resident_bytes: usize,
    pub byte_budget: usize,
    pub entry_limit: usize,
}

#[derive(Debug, Clone)]
struct PageSlot {
    page_id: u32,
    generation: u64,
    bytes: Arc<[u8]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CacheKey {
    slot: u8,
    generation: u64,
    request: TextureRequest,
}

#[derive(Debug)]
struct CacheEntry {
    handle: TextureHandle,
    pixels: Arc<DecodedTexture>,
    content_uv: TextureUvBounds,
    byte_len: usize,
    last_used: u64,
}

/// Safe replacement for the fixed hash tables and owned texture arena.
#[derive(Debug)]
pub struct TextureCache {
    live_pages: [Option<PageSlot>; TEXTURE_PAGE_SLOT_COUNT],
    frame_pages: [Option<PageSlot>; TEXTURE_PAGE_SLOT_COUNT],
    entries: HashMap<CacheKey, CacheEntry>,
    byte_budget: usize,
    entry_limit: usize,
    resident_bytes: usize,
    next_generation: u64,
    next_handle: u64,
    use_clock: u64,
    frame: CacheCounters,
    total: CacheCounters,
}

impl Default for TextureCache {
    fn default() -> Self {
        Self::with_limits(DEFAULT_TEXTURE_BUDGET_BYTES, DEFAULT_TEXTURE_ENTRY_LIMIT)
    }
}

impl TextureCache {
    #[must_use]
    pub fn with_limits(byte_budget: usize, entry_limit: usize) -> Self {
        Self {
            live_pages: std::array::from_fn(|_| None),
            frame_pages: std::array::from_fn(|_| None),
            entries: HashMap::new(),
            byte_budget,
            entry_limit,
            resident_bytes: 0,
            next_generation: 1,
            next_handle: 1,
            use_clock: 0,
            frame: CacheCounters::default(),
            total: CacheCounters::default(),
        }
    }

    /// Install a decompressed 64 KiB page into one of eight live slots.
    ///
    /// A new generation is assigned even when an EID returns to its former
    /// slot, preventing stale decoded pixels from a previous level visit.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid slot, a page not exactly 64 KiB long,
    /// or exhaustion of the monotonically increasing generation counter.
    pub fn install_page(
        &mut self,
        slot: usize,
        page_id: u32,
        bytes: Vec<u8>,
    ) -> Result<u64, CacheError> {
        if slot >= TEXTURE_PAGE_SLOT_COUNT {
            return Err(CacheError::InvalidSlot(slot));
        }
        if bytes.len() != TEXTURE_PAGE_BYTES {
            return Err(CacheError::Texture(TextureError::InvalidPageLength {
                actual: bytes.len(),
            }));
        }
        let generation = self.next_generation;
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(CacheError::GenerationExhausted)?;
        self.live_pages[slot] = Some(PageSlot {
            page_id,
            generation,
            bytes: Arc::from(bytes),
        });
        Ok(generation)
    }

    /// Remove a live page mapping.
    ///
    /// # Errors
    ///
    /// Returns [`CacheError::InvalidSlot`] when `slot` is outside `0..8`.
    pub fn remove_page(&mut self, slot: usize) -> Result<(), CacheError> {
        if slot >= TEXTURE_PAGE_SLOT_COUNT {
            return Err(CacheError::InvalidSlot(slot));
        }
        self.live_pages[slot] = None;
        Ok(())
    }

    /// Freeze live page mappings for one render frame and retire old entries.
    pub fn begin_frame(&mut self) {
        self.frame = CacheCounters::default();
        for slot in 0..TEXTURE_PAGE_SLOT_COUNT {
            let live_generation = self.live_pages[slot].as_ref().map(|page| page.generation);
            let frame_generation = self.frame_pages[slot].as_ref().map(|page| page.generation);
            if live_generation == frame_generation {
                continue;
            }
            increment(&mut self.frame.page_changes);
            increment(&mut self.total.page_changes);
            let keys: Vec<_> = self
                .entries
                .keys()
                .filter(|key| {
                    usize::from(key.slot) == slot && Some(key.generation) != live_generation
                })
                .copied()
                .collect();
            for key in keys {
                self.remove_entry(&key);
                increment(&mut self.frame.generation_invalidations);
                increment(&mut self.total.generation_invalidations);
            }
            self.frame_pages[slot].clone_from(&self.live_pages[slot]);
        }
    }

    /// Resolve, decode, and cache one texture request.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested page is absent or changed during
    /// the frame, its region/CLUT is invalid, or the decoded image cannot fit
    /// within the configured cache limits.
    pub fn load(&mut self, request: TextureRequest) -> Result<CachedTexture, CacheError> {
        increment(&mut self.frame.requests);
        increment(&mut self.total.requests);
        self.use_clock = self.use_clock.saturating_add(1);

        let frame_slot = find_page(&self.frame_pages, request.page_id);
        if let Some((slot, frame_page)) =
            frame_slot.and_then(|slot| self.frame_pages[slot].as_ref().map(|page| (slot, page)))
        {
            let key = CacheKey {
                slot: u8::try_from(slot).unwrap_or_default(),
                generation: frame_page.generation,
                request,
            };
            if let Some(entry) = self.entries.get_mut(&key) {
                entry.last_used = self.use_clock;
                increment(&mut self.frame.hits);
                increment(&mut self.total.hits);
                return Ok(CachedTexture {
                    handle: entry.handle,
                    page_id: request.page_id,
                    page_generation: key.generation,
                    pixels: Arc::clone(&entry.pixels),
                    content_uv: entry.content_uv,
                });
            }
        }

        increment(&mut self.frame.misses);
        increment(&mut self.total.misses);
        let Some(live_slot) = find_page(&self.live_pages, request.page_id) else {
            self.record_missing_page();
            return Err(CacheError::MissingPage(request.page_id));
        };
        let Some(live_generation) = self.live_pages[live_slot]
            .as_ref()
            .map(|page| page.generation)
        else {
            self.record_generation_miss();
            return Err(CacheError::GenerationChanged {
                page_id: request.page_id,
            });
        };
        let Some(frame_page) = self.frame_pages[live_slot].clone() else {
            self.record_generation_miss();
            return Err(CacheError::GenerationChanged {
                page_id: request.page_id,
            });
        };
        if frame_slot != Some(live_slot) || frame_page.generation != live_generation {
            self.record_generation_miss();
            return Err(CacheError::GenerationChanged {
                page_id: request.page_id,
            });
        }

        let palette = request.clut.map(Palette::Page);
        let decoded = crate::texture::decode_region(
            &frame_page.bytes,
            request.color_mode,
            request.blend_mode,
            request.region,
            palette,
        )
        .and_then(|texture| texture.with_edge_padding(1))
        .map_err(|error| {
            self.record_cache_failure();
            CacheError::Texture(error)
        })?;
        let byte_len = decoded.byte_len();
        if byte_len > self.byte_budget || self.entry_limit == 0 {
            self.record_cache_failure();
            return Err(CacheError::BudgetExceeded {
                requested: byte_len,
                budget: self.byte_budget,
            });
        }
        while self.entries.len() >= self.entry_limit
            || self.resident_bytes.saturating_add(byte_len) > self.byte_budget
        {
            let Some(key) = self.least_recently_used_key() else {
                self.record_cache_failure();
                return Err(CacheError::BudgetExceeded {
                    requested: byte_len,
                    budget: self.byte_budget,
                });
            };
            self.remove_entry(&key);
            increment(&mut self.frame.evictions);
            increment(&mut self.total.evictions);
        }

        let handle = TextureHandle(self.next_handle);
        self.next_handle = self.next_handle.saturating_add(1);
        let pixels = Arc::new(decoded);
        let content_uv = content_uv(request.region, &pixels);
        let key = CacheKey {
            slot: u8::try_from(live_slot).unwrap_or_default(),
            generation: frame_page.generation,
            request,
        };
        self.resident_bytes = self.resident_bytes.saturating_add(byte_len);
        self.entries.insert(
            key,
            CacheEntry {
                handle,
                pixels: Arc::clone(&pixels),
                content_uv,
                byte_len,
                last_used: self.use_clock,
            },
        );
        add_bytes(&mut self.frame.upload_bytes, byte_len);
        add_bytes(&mut self.total.upload_bytes, byte_len);
        Ok(CachedTexture {
            handle,
            page_id: request.page_id,
            page_generation: frame_page.generation,
            pixels,
            content_uv,
        })
    }

    #[must_use]
    pub fn metrics(&self) -> CacheMetrics {
        CacheMetrics {
            frame: self.frame.clone(),
            total: self.total.clone(),
            resident_entries: self.entries.len(),
            resident_bytes: self.resident_bytes,
            byte_budget: self.byte_budget,
            entry_limit: self.entry_limit,
        }
    }

    #[must_use]
    pub fn page_generation(&self, slot: usize) -> Option<u64> {
        self.live_pages
            .get(slot)
            .and_then(Option::as_ref)
            .map(|page| page.generation)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.resident_bytes = 0;
        self.frame = CacheCounters::default();
        self.total = CacheCounters::default();
    }

    fn record_missing_page(&mut self) {
        increment(&mut self.frame.failures);
        increment(&mut self.total.failures);
        increment(&mut self.frame.missing_pages);
        increment(&mut self.total.missing_pages);
    }

    fn record_generation_miss(&mut self) {
        increment(&mut self.frame.failures);
        increment(&mut self.total.failures);
        increment(&mut self.frame.generation_misses);
        increment(&mut self.total.generation_misses);
    }

    fn record_cache_failure(&mut self) {
        increment(&mut self.frame.failures);
        increment(&mut self.total.failures);
        increment(&mut self.frame.cache_failures);
        increment(&mut self.total.cache_failures);
    }

    fn least_recently_used_key(&self) -> Option<CacheKey> {
        self.entries
            .iter()
            .min_by_key(|(key, entry)| (entry.last_used, entry.handle, key.generation))
            .map(|(key, _)| *key)
    }

    fn remove_entry(&mut self, key: &CacheKey) {
        if let Some(entry) = self.entries.remove(key) {
            self.resident_bytes = self.resident_bytes.saturating_sub(entry.byte_len);
        }
    }
}

fn find_page(pages: &[Option<PageSlot>; TEXTURE_PAGE_SLOT_COUNT], page_id: u32) -> Option<usize> {
    pages
        .iter()
        .position(|slot| slot.as_ref().is_some_and(|page| page.page_id == page_id))
}

fn content_uv(region: TextureRegion, texture: &DecodedTexture) -> TextureUvBounds {
    let width = f32::from(u16::try_from(texture.width()).unwrap_or(u16::MAX));
    let height = f32::from(u16::try_from(texture.height()).unwrap_or(u16::MAX));
    let region_width = f32::from(u16::try_from(region.width).unwrap_or(u16::MAX));
    let region_height = f32::from(u16::try_from(region.height).unwrap_or(u16::MAX));
    TextureUvBounds {
        left: 1.5 / width,
        top: 1.5 / height,
        right: (region_width + 0.5) / width,
        bottom: (region_height + 0.5) / height,
    }
}

fn increment(counter: &mut u64) {
    *counter = counter.saturating_add(1);
}

fn add_bytes(counter: &mut u64, bytes: usize) {
    *counter = counter.saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    InvalidSlot(usize),
    GenerationExhausted,
    MissingPage(u32),
    GenerationChanged { page_id: u32 },
    BudgetExceeded { requested: usize, budget: usize },
    Texture(TextureError),
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSlot(slot) => {
                write!(formatter, "texture page slot {slot} is outside 0..8")
            }
            Self::GenerationExhausted => {
                formatter.write_str("texture page generation counter exhausted")
            }
            Self::MissingPage(page_id) => {
                write!(formatter, "texture page {page_id:#010x} is not mounted")
            }
            Self::GenerationChanged { page_id } => write!(
                formatter,
                "texture page {page_id:#010x} changed after the frame snapshot"
            ),
            Self::BudgetExceeded { requested, budget } => write!(
                formatter,
                "decoded texture needs {requested} bytes, exceeding {budget}-byte cache budget"
            ),
            Self::Texture(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Texture(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn direct_page(color: u16) -> Vec<u8> {
        color
            .to_le_bytes()
            .into_iter()
            .cycle()
            .take(TEXTURE_PAGE_BYTES)
            .collect()
    }

    fn request(page_id: u32, x: u32) -> TextureRequest {
        TextureRequest {
            page_id,
            region: TextureRegion::new(x, 0, 4, 4).unwrap(),
            color_mode: ColorMode::Direct15,
            blend_mode: BlendMode::Opaque,
            clut: None,
        }
    }

    #[test]
    fn cache_hits_and_upload_metrics_match_padded_region() {
        let mut cache = TextureCache::default();
        cache.install_page(0, 0x101, direct_page(0xffff)).unwrap();
        cache.begin_frame();
        let first = cache.load(request(0x101, 0)).unwrap();
        assert_eq!((first.pixels.width(), first.pixels.height()), (6, 6));
        let metrics = cache.metrics();
        assert_eq!(metrics.frame.requests, 1);
        assert_eq!(metrics.frame.misses, 1);
        assert_eq!(metrics.frame.hits, 0);
        assert_eq!(metrics.frame.upload_bytes, 6 * 6 * 4);

        let second = cache.load(request(0x101, 0)).unwrap();
        assert_eq!(second.handle, first.handle);
        let metrics = cache.metrics();
        assert_eq!(
            metrics.frame.requests,
            metrics.frame.hits + metrics.frame.misses
        );
        assert_eq!(metrics.frame.hits, 1);
        assert_eq!(metrics.frame.upload_bytes, 6 * 6 * 4);
    }

    #[test]
    fn replacement_is_frame_stable_then_invalidated() {
        let mut cache = TextureCache::default();
        cache.install_page(0, 0x101, direct_page(0xffff)).unwrap();
        cache.begin_frame();
        let old = cache.load(request(0x101, 0)).unwrap();

        cache.install_page(0, 0x103, direct_page(0x83e0)).unwrap();
        // A cached command from this frame retains its old generation.
        assert_eq!(cache.load(request(0x101, 0)).unwrap().handle, old.handle);
        assert_eq!(cache.metrics().frame.generation_misses, 0);

        cache.begin_frame();
        assert!(matches!(
            cache.load(request(0x101, 0)),
            Err(CacheError::MissingPage(0x101))
        ));
        assert_eq!(cache.metrics().frame.missing_pages, 1);
        assert_eq!(cache.metrics().resident_entries, 0);
    }

    #[test]
    fn newly_installed_mid_frame_page_reports_generation_miss() {
        let mut cache = TextureCache::default();
        cache.begin_frame();
        cache.install_page(0, 0x101, direct_page(0xffff)).unwrap();
        assert!(matches!(
            cache.load(request(0x101, 0)),
            Err(CacheError::GenerationChanged { page_id: 0x101 })
        ));
        assert_eq!(cache.metrics().frame.generation_misses, 1);
    }

    #[test]
    fn returning_eid_gets_a_fresh_generation() {
        let mut cache = TextureCache::default();
        let first_generation = cache.install_page(0, 0x101, direct_page(0xffff)).unwrap();
        cache.begin_frame();
        let first_handle = cache.load(request(0x101, 0)).unwrap().handle;
        cache.install_page(0, 0x103, direct_page(0x83e0)).unwrap();
        cache.begin_frame();
        cache.load(request(0x103, 0)).unwrap();
        let returning_generation = cache.install_page(0, 0x101, direct_page(0x001f)).unwrap();
        cache.begin_frame();
        let returning_handle = cache.load(request(0x101, 0)).unwrap().handle;
        assert!(returning_generation > first_generation);
        assert_ne!(returning_handle, first_handle);
        assert!(cache.metrics().total.page_changes >= 3);
    }

    #[test]
    fn lru_eviction_holds_the_configured_budget() {
        // Each padded 4x4 region is 6x6x4 = 144 bytes.
        let mut cache = TextureCache::with_limits(144, 8);
        cache.install_page(0, 0x101, direct_page(0xffff)).unwrap();
        cache.begin_frame();
        let first = cache.load(request(0x101, 0)).unwrap();
        let second = cache.load(request(0x101, 4)).unwrap();
        assert_ne!(first.handle, second.handle);
        let metrics = cache.metrics();
        assert_eq!(metrics.resident_entries, 1);
        assert_eq!(metrics.resident_bytes, 144);
        assert_eq!(metrics.frame.evictions, 1);
        let reloaded = cache.load(request(0x101, 0)).unwrap();
        assert_ne!(reloaded.handle, first.handle);
        assert_eq!(cache.metrics().frame.evictions, 2);
    }

    #[test]
    fn over_budget_single_texture_fails_without_growing() {
        let mut cache = TextureCache::with_limits(143, 8);
        cache.install_page(0, 0x101, direct_page(0xffff)).unwrap();
        cache.begin_frame();
        assert!(matches!(
            cache.load(request(0x101, 0)),
            Err(CacheError::BudgetExceeded {
                requested: 144,
                budget: 143
            })
        ));
        let metrics = cache.metrics();
        assert_eq!(metrics.resident_bytes, 0);
        assert_eq!(metrics.frame.cache_failures, 1);
    }

    proptest! {
        #[test]
        fn resident_usage_never_exceeds_arbitrary_budget(budget in 0_usize..4096, xs in prop::collection::vec(0_u8..64, 0..32)) {
            let mut cache = TextureCache::with_limits(budget, 16);
            cache.install_page(0, 1, direct_page(0x7fff)).unwrap();
            cache.begin_frame();
            for x in xs {
                let _ = cache.load(TextureRequest {
                    page_id: 1,
                    region: TextureRegion::new(u32::from(x), 0, 1, 1).unwrap(),
                    color_mode: ColorMode::Direct15,
                    blend_mode: BlendMode::Opaque,
                    clut: None,
                });
                prop_assert!(cache.metrics().resident_bytes <= budget);
                prop_assert!(cache.metrics().resident_entries <= 16);
            }
        }
    }
}
