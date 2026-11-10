// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

//! Font handling and caching.

use ab_glyph::{FontArc, FontRef, FontVec};
use anyhow::{Context, anyhow};
use log::warn;
use once_cell::sync::Lazy;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

static DEFAULT_TTF_FONT: Lazy<FontArc> = Lazy::new(|| {
    FontArc::new(
        FontRef::try_from_slice(include_bytes!("../../../fonts/DejaVuSans.ttf"))
            .expect("Failed to load default font"),
    )
});

pub struct FontHandler {
    ttf_path: PathBuf,
    ttf_cache: HashMap<String, FontArc>,
    /// Fonts that failed to load; [`FontHandler::get_ttf_font_or_default`]
    /// uses the default font for them without retrying (and warning) on
    /// every rendered text.
    missing: HashSet<String>,
}

impl FontHandler {
    pub fn new(ttf_path: impl Into<PathBuf>) -> Self {
        Self {
            ttf_path: ttf_path.into(),
            ttf_cache: Default::default(),
            missing: Default::default(),
        }
    }

    pub fn default_font() -> FontArc {
        DEFAULT_TTF_FONT.clone()
    }

    /// Like [`FontHandler::get_ttf_font`], but falls back to the default font.
    ///
    /// A font that fails to load is remembered: the warning is logged once
    /// and later calls return the default font without touching the file
    /// system again (until [`FontHandler::clear`]).
    pub fn get_ttf_font_or_default(&mut self, name: &str) -> FontArc {
        if self.missing.contains(name) {
            return FontHandler::default_font();
        }
        self.get_ttf_font(name).unwrap_or_else(|e| {
            warn!("Failed to load font: {e}. Using default");
            self.missing.insert(name.to_string());
            FontHandler::default_font()
        })
    }

    pub fn get_ttf_font(&mut self, name: &str) -> anyhow::Result<FontArc> {
        if let Some(font) = self.ttf_cache.get(name) {
            return Ok(font.clone());
        }
        let mut path = self.ttf_path.join(name);
        path.set_extension("ttf");

        if !path.exists() {
            return Err(anyhow!("{name}.ttf not found"));
        }

        let data = fs::read(path).with_context(|| format!("Error reading font {name}.ttf"))?;
        let font = FontArc::new(
            FontVec::try_from_vec(data)
                .with_context(|| format!("Error parsing font {name}.ttf"))?,
        );

        self.ttf_cache.insert(name.to_string(), font.clone());

        Ok(font)
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.ttf_cache.clear();
        self.missing.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_font_falls_back_and_is_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let mut fonts = FontHandler::new(dir.path());
        assert!(fonts.get_ttf_font("NoSuchFont").is_err());
        let _ = fonts.get_ttf_font_or_default("NoSuchFont");
        assert!(fonts.missing.contains("NoSuchFont"));

        // Remembered: even a font file appearing later is not loaded
        // (no per-frame file system probing for a missing font).
        fs::copy(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../fonts/DejaVuSans.ttf"),
            dir.path().join("NoSuchFont.ttf"),
        )
        .unwrap();
        let _ = fonts.get_ttf_font_or_default("NoSuchFont");
        assert!(!fonts.ttf_cache.contains_key("NoSuchFont"));

        // clear() forgets it, so the next lookup loads the file
        fonts.clear();
        let _ = fonts.get_ttf_font_or_default("NoSuchFont");
        assert!(fonts.ttf_cache.contains_key("NoSuchFont"));
        assert!(!fonts.missing.contains("NoSuchFont"));
    }
}
