// SPDX-License-Identifier: MIT OR Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025 Markus Zehnder

//! Sensor panel rendering logic. Create an RGBa image from a panel configuration and sensor values.

use crate::cfg::{Panel, Sensor, SensorDirection, SensorMode, TextAlign};
use crate::font::FontHandler;
use crate::format_value;
use crate::img::{ImageCache, Size, rotate_image};
use crate::sensors::get_date_time_value;
use ab_glyph::Font;
use chrono::{DateTime, Local};
use image::{Rgba, RgbaImage};
use imageproc::drawing::{draw_text_mut, text_size};
use log::{debug, error, info, warn};
use std::borrow::Cow;
use std::collections::HashMap;
use std::f32::consts::PI;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;
use std::time::Instant;

/// Error type for image processing operations
#[derive(Debug)]
#[allow(dead_code)]
pub enum ImageProcessingError {
    ImageLoadError(String),
    InvalidMode(i32),
    InvalidDirection(SensorDirection),
    MathError(String),
    IoError(std::io::Error),
}

impl From<std::io::Error> for ImageProcessingError {
    fn from(error: std::io::Error) -> Self {
        ImageProcessingError::IoError(error)
    }
}

/// Pixel rectangle `[x0, x1) x [y0, y1)`, clipped to the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

/// Overlay layer for one graphic sensor mode (fan, progress, pointer).
///
/// The layer buffer is kept across frames. Every draw records its target
/// rectangle in `dirty`, so the next frame only clears (and compositing only
/// visits) the area that was drawn, instead of reallocating and scanning the
/// whole panel-sized layer every frame. Outside `dirty` the layer is always
/// fully transparent.
struct Layer {
    image: RgbaImage,
    dirty: Option<Rect>,
}

impl Layer {
    /// Record a draw to the `w` x `h` rectangle at (`x`, `y`), clipped to the layer.
    fn mark(&mut self, x: i32, y: i32, w: u32, h: u32) {
        let (lw, lh) = self.image.dimensions();
        let clip = |v: i64, max: u32| v.clamp(0, max as i64) as u32;
        let rect = Rect {
            x0: clip(x as i64, lw),
            y0: clip(y as i64, lh),
            x1: clip(x as i64 + w as i64, lw),
            y1: clip(y as i64 + h as i64, lh),
        };
        if rect.x0 >= rect.x1 || rect.y0 >= rect.y1 {
            return;
        }
        self.dirty = Some(match self.dirty {
            None => rect,
            Some(d) => Rect {
                x0: d.x0.min(rect.x0),
                y0: d.y0.min(rect.y0),
                x1: d.x1.max(rect.x1),
                y1: d.y1.max(rect.y1),
            },
        });
    }

    /// Make the layer fully transparent again.
    fn clear(&mut self) {
        if let Some(r) = self.dirty.take() {
            for y in r.y0..r.y1 {
                for x in r.x0..r.x1 {
                    self.image.put_pixel(x, y, Rgba([0, 0, 0, 0]));
                }
            }
        }
    }
}

/// Graphic sensor modes with an overlay layer, in compositing order.
const LAYER_MODES: [SensorMode; 3] = [SensorMode::Fan, SensorMode::Progress, SensorMode::Pointer];

/// Sensor panel renderer.
///
/// Renders a final display image from a sensor panel configuration and current sensor values.
/// All defined fonts and images of a sensor panel are cached after first use.
pub struct PanelRenderer {
    size: Size,
    /// Rendered panel image, reused across frames.
    frame: RgbaImage,
    /// Overlay layers in [`LAYER_MODES`] order, allocated on first use.
    layers: [Option<Layer>; 3],
    /// Sensors that failed to render, keyed by sensor instance (address),
    /// with the last logged error message. Used to log a failing sensor once
    /// instead of on every frame. Keyed per instance, not per label: a text
    /// and a progress sensor commonly share one label, and only one of them
    /// may fail for a given value.
    sensor_errors: HashMap<usize, String>,
    font_handler: FontHandler,
    image_cache: ImageCache,
    // for debugging: save images for inspection
    save_render_img: bool,
    save_processed_pic: bool,
    save_progress_layer: bool,
    img_save_path: PathBuf,
    img_suffix: Option<String>,
}

impl PanelRenderer {
    /// Create a new image processor instance for a given display size.
    ///
    /// # Arguments
    ///
    /// * `size`: display size, used to render a panel image.
    /// * `font_dir`: font directory to load TTF fonts specified in a sensor configuration.
    /// * `img_dir`: image directory to load background and sensor images from.
    ///
    /// returns: PanelRenderer
    pub fn new(size: Size, font_dir: impl Into<PathBuf>, img_dir: impl Into<PathBuf>) -> Self {
        Self {
            size,
            frame: RgbaImage::new(0, 0),
            layers: [None, None, None],
            sensor_errors: HashMap::new(),
            font_handler: FontHandler::new(font_dir),
            image_cache: ImageCache::new(img_dir),
            save_render_img: false,
            save_processed_pic: false,
            save_progress_layer: false,
            img_save_path: PathBuf::from("out"),
            img_suffix: None,
        }
    }

    /// For debugging: save rendered panel image as .PNG graphic for inspection.
    pub fn set_save_render_img(&mut self, save: bool) {
        self.save_render_img = save;
        self.create_img_save_path();
    }
    /// For debugging: save all processed sensor pic images as .PNG graphics for inspection.
    pub fn set_save_processed_pic(&mut self, save: bool) {
        self.save_processed_pic = save;
        self.create_img_save_path();
    }
    /// For debugging: save all progress layer images as .PNG graphics for inspection.
    pub fn set_save_progress_layer(&mut self, save: bool) {
        self.save_progress_layer = save;
        self.create_img_save_path();
    }
    /// Set output directory path for saving images.
    ///
    /// Default output directory is `./out` in the current working directory.
    pub fn set_img_save_path(&mut self, img_dir: impl Into<PathBuf>) {
        self.img_save_path = img_dir.into();
        self.create_img_save_path();
    }
    /// Set an optional image name suffix for saving a .PNG graphic file.
    ///
    /// This function needs to be called before [render()] if a different suffix should be used for each rendered panel.
    pub fn set_img_suffix(&mut self, img_suffix: impl Into<String>) {
        self.img_suffix = Some(img_suffix.into());
    }

    /// Render a sensor panel with the given values and return the final panel image.
    ///
    /// `DATE_*` sensors without a value in `values` show the current local time.
    ///
    /// # Arguments
    ///
    /// * `panel`: the panel configuration
    /// * `values`: current values for the defined panel sensors in a shared HashMap
    ///
    /// returns: the rendered panel image, or an [ImageProcessingError] in case of an error.
    /// The image buffer is owned by the renderer and reused for the next frame.
    pub fn render(
        &mut self,
        panel: &Panel,
        values: &HashMap<String, String>,
    ) -> Result<&RgbaImage, ImageProcessingError> {
        self.render_at(panel, values, &Local::now())
    }

    /// Like [`PanelRenderer::render`], with an explicit time for `DATE_*` sensors.
    ///
    /// Use the same `now` for [`PanelRenderer::content_key`] to decide whether a frame changed.
    pub fn render_at(
        &mut self,
        panel: &Panel,
        values: &HashMap<String, String>,
        now: &DateTime<Local>,
    ) -> Result<&RgbaImage, ImageProcessingError> {
        debug!(
            "Rendering panel {}...",
            panel
                .name
                .as_deref()
                .unwrap_or_else(|| panel.id.as_deref().unwrap_or_default())
        );

        let start = Instant::now();
        let mut frame = std::mem::take(&mut self.frame);
        let (w, h) = self.size;
        if let Some(img) = &panel.img
            && let Some(background) = self.image_cache.get(img, Some(self.size))
        {
            if background.dimensions() == frame.dimensions() {
                frame.copy_from_slice(background);
            } else {
                // e.g. the same file was cached earlier at another size
                frame.clone_from(background);
            }
        } else if frame.dimensions() == (w, h) {
            frame.fill(0);
        } else {
            frame = RgbaImage::new(w, h);
        }

        self.render_sensors(panel, values, now, &mut frame);
        self.frame = frame;

        debug!("Rendered panel in {}ms", start.elapsed().as_millis());

        if self.save_render_img {
            let name = format!(
                "render_{}{}.png",
                panel.friendly_name(),
                self.img_suffix.as_deref().unwrap_or_default()
            );
            if let Err(e) = self.frame.save(self.img_save_path.join(name)) {
                error!("Error saving rendered panel image: {e}");
            }
        }

        Ok(&self.frame)
    }

    /// Render all panel sensors with the given values on a background image
    pub fn render_all_sensors(
        &mut self,
        panel: &Panel,
        values: &HashMap<String, String>,
        mut background: RgbaImage,
    ) -> Result<RgbaImage, ImageProcessingError> {
        self.render_sensors(panel, values, &Local::now(), &mut background);
        Ok(background)
    }

    /// Cheap fingerprint of everything that changes the rendered image of `panel`:
    /// the displayed text of text sensors and the raw value of graphic sensors,
    /// with `DATE_*` sensors resolved at `now`.
    ///
    /// Two calls with the same panel and equal keys render identical images, so
    /// an unchanged key means the frame does not need to be rendered and sent again.
    pub fn content_key(
        &self,
        panel: &Panel,
        values: &HashMap<String, String>,
        now: &DateTime<Local>,
    ) -> u64 {
        let mut hasher = DefaultHasher::new();
        panel.sensor.len().hash(&mut hasher);
        for sensor in &panel.sensor {
            let value = resolved_sensor_value(sensor, values, now);
            match (&value, sensor.mode) {
                (Some(value), SensorMode::Text) => {
                    let unit = resolved_sensor_unit(sensor, values);
                    Some(sensor_text(sensor, value, &unit)).hash(&mut hasher);
                }
                _ => value.hash(&mut hasher),
            }
        }
        hasher.finish()
    }

    /// Render all sensors of `panel` onto `background`, then composite the layers.
    ///
    /// A sensor that fails to render (e.g. a non-numeric progress value) is
    /// skipped and logged once; the remaining sensors are still rendered.
    fn render_sensors(
        &mut self,
        panel: &Panel,
        values: &HashMap<String, String>,
        now: &DateTime<Local>,
        background: &mut RgbaImage,
    ) {
        for layer in self.layers.iter_mut().flatten() {
            layer.clear();
        }

        for sensor in &panel.sensor {
            let unit = resolved_sensor_unit(sensor, values);

            if let Some(value) = resolved_sensor_value(sensor, values, now) {
                let result = self.render_sensor(background, sensor, &value, &unit);
                self.track_sensor_result(sensor, &value, result);
            }
        }

        // Final compositing
        self.composite_layers(background);
    }

    /// Log a sensor render failure once (until the error changes or the
    /// sensor renders again). Returns `true` when something was logged.
    fn track_sensor_result(
        &mut self,
        sensor: &Sensor,
        value: &str,
        result: Result<(), ImageProcessingError>,
    ) -> bool {
        let id = sensor as *const Sensor as usize;
        match result {
            Ok(()) => {
                let recovered = self.sensor_errors.remove(&id).is_some();
                if recovered {
                    info!("Sensor '{}' renders again", sensor.label);
                }
                recovered
            }
            Err(e) => {
                let msg = format!("{e:?}");
                if self.sensor_errors.get(&id) == Some(&msg) {
                    return false;
                }
                warn!(
                    "Skipping sensor '{}' (value '{value}'): {msg}",
                    sensor.label
                );
                self.sensor_errors.insert(id, msg);
                true
            }
        }
    }

    /// Render a single sensor element based on its mode
    fn render_sensor(
        &mut self,
        background: &mut RgbaImage,
        sensor: &Sensor,
        value: &str,
        unit: &str,
    ) -> Result<(), ImageProcessingError> {
        let direction = sensor.direction.unwrap_or(SensorDirection::LeftToRight);

        match sensor.mode {
            SensorMode::Text => self.render_text(background, sensor, value, unit),
            SensorMode::Fan => self.render_fan(sensor, value, direction),
            SensorMode::Progress => self.render_progress(sensor, value, direction),
            SensorMode::Pointer => self.render_pointer(sensor, value, direction),
        }
    }

    /// Mode 1 - Text
    fn render_text(
        &mut self,
        background: &mut RgbaImage,
        sensor: &Sensor,
        value: &str,
        unit: &str,
    ) -> Result<(), ImageProcessingError> {
        let font = if let Some(font_family) = &sensor.font_family {
            self.font_handler.get_ttf_font_or_default(font_family)
        } else {
            FontHandler::default_font()
        };
        let font_size = sensor.font_size.unwrap_or(14) as f32;
        // TODO verify pixel scaling! Is font_size point size or pixel size?
        // TODO some font size calculation is missing, dpi scaling? internal padding?
        //      The adjustment hack is required to get the correct size of the rendered text.
        //      However, the y-position requires the regular value (see multiplication by 1.33 below)
        let adjustment_hack = 0.75;
        let scale = font.pt_to_px_scale(font_size * adjustment_hack).unwrap();

        let text = sensor_text(sensor, value, unit);
        let size = text_size(scale, &font, &text);
        let width = sensor.width.unwrap_or_default() as i32;
        let height = sensor.height.unwrap_or_default() as i32;
        let x = match sensor.text_align.unwrap_or_default() {
            TextAlign::Left => sensor.x,
            TextAlign::Center => sensor.x + width / 2 - (size.0 / 2) as i32,
            TextAlign::Right => sensor.x + width - size.0 as i32,
        };
        // FIXME figure out font scaling factor / padding / dpi etc. See above for y-adjustment hack.
        // This work quite ok for most panels, but not all!
        // Some work better with `sensor.y + height / 2 - size.1 as i32;`
        // The y parameter in `draw_text_mut` is still a mystery: drawing text at position (0,0)
        // renders a huge gap at the top, about the size of half the font-height!?
        let y = sensor.y + height / 2 - (size.1 as f32 * 1.3333 / 2f32) as i32;

        debug!(
            "Sensor({:03},{:03}), pixel({x:03},{y:03}), size{size:?}: {text}",
            sensor.x, sensor.y
        );

        let font_color = sensor.font_color.unwrap_or_default().into();
        draw_text_mut(background, font_color, x, y, scale, &font, &text);

        Ok(())
    }

    /// Mode 2 - Circular/Arc progress indicator
    fn render_fan(
        &mut self,
        sensor: &Sensor,
        value: &str,
        direction: SensorDirection,
    ) -> Result<(), ImageProcessingError> {
        if !matches!(
            direction,
            SensorDirection::LeftToRight | SensorDirection::RightToLeft
        ) {
            return Err(ImageProcessingError::InvalidDirection(direction));
        }

        let pos_x = sensor.x;
        let pos_y = sensor.y;

        let pic_path = sensor.pic.as_ref().ok_or_else(|| {
            ImageProcessingError::ImageLoadError("No picture specified".to_string())
        })?;

        let min_angle = sensor.min_angle.unwrap_or(0) as f32;
        let max_angle = sensor.max_angle.unwrap_or(180) as f32;
        let min_value = sensor.min_value.unwrap_or(0.0);
        let max_value = sensor.max_value.unwrap_or(100.0);

        // borrow the cached pic and the layer separately instead of cloning the pic
        let target_image = self.image_cache.get(pic_path, None).ok_or_else(|| {
            ImageProcessingError::ImageLoadError(format!("Failed to load: {:?}", pic_path))
        })?;

        let current_value = value
            .parse::<f32>()
            .map_err(|_| ImageProcessingError::MathError("Invalid value".to_string()))?;

        if current_value <= min_value {
            return Ok(());
        }

        let progress = if current_value >= max_value {
            1.0
        } else {
            (current_value - min_value) / (max_value - min_value)
        };

        let (start_angle, end_angle) = if direction == SensorDirection::LeftToRight {
            // Clockwise
            let start = min_angle - 90.0;
            let end = min_angle + (max_angle - min_angle) * progress - 90.0;
            (start, end)
        } else {
            // Counter-clockwise
            let start = 360.0 - min_angle - (max_angle - min_angle) * progress - 90.0;
            let end = 360.0 - min_angle - 90.0;
            (start, end)
        };

        let sector_layer = Self::layer(&mut self.layers, self.size, SensorMode::Fan);
        PanelRenderer::draw_pie_slice(
            &mut sector_layer.image,
            target_image,
            pos_x,
            pos_y,
            start_angle,
            end_angle,
        );
        // superset of the pixels draw_pie_slice can touch
        let (src_w, src_h) = target_image.dimensions();
        sector_layer.mark(
            pos_x - src_w as i32 / 2,
            pos_y - src_h as i32 / 2,
            src_w,
            src_h,
        );

        Ok(())
    }

    /// Mode 3 - render progress graphic based on percentage value.
    ///
    /// The progress graphic must show the 100% value and is cut based on the actual value.
    fn render_progress(
        &mut self,
        sensor: &Sensor,
        value: &str,
        direction: SensorDirection,
    ) -> Result<(), ImageProcessingError> {
        let pic_path = sensor.pic.as_ref().ok_or_else(|| {
            ImageProcessingError::ImageLoadError("No picture specified".to_string())
        })?;

        let pic = self.image_cache.get(pic_path, None).ok_or_else(|| {
            ImageProcessingError::ImageLoadError(format!("Failed to load: {:?}", pic_path))
        })?;

        let min_val = sensor.min_value.unwrap_or(0.0);
        let max_val = sensor.max_value.unwrap_or(100.0);

        let current_value = value
            .parse::<f32>()
            .map_err(|_| ImageProcessingError::MathError("Invalid value".to_string()))?;

        let clamped_value = current_value.clamp(min_val, max_val);
        let progress = ((clamped_value - min_val) / (max_val - min_val)).clamp(0.0, 1.0);

        let (img_w, img_h) = pic.dimensions();

        // Visible part of the progress graphic as (x0, y0, x1, y1), based on direction
        let crop_rect = match direction {
            SensorDirection::LeftToRight => {
                let crop_w = (img_w as f32 * progress).round() as u32;
                (0, 0, crop_w, img_h)
            }
            SensorDirection::RightToLeft => {
                let crop_w = (img_w as f32 * progress).round() as u32;
                (img_w - crop_w, 0, img_w, img_h)
            }
            SensorDirection::TopToBottom => {
                let crop_h = (img_h as f32 * progress).round() as u32;
                (0, 0, img_w, crop_h)
            }
            SensorDirection::BottomToTop => {
                let crop_h = (img_h as f32 * progress).round() as u32;
                (0, img_h - crop_h, img_w, img_h)
            }
        };

        let pos_x = sensor.x;
        let pos_y = sensor.y;

        // Only the visible part is pasted: the masked-out pixels of the full
        // graphic are fully transparent, and pasting a transparent pixel
        // leaves the layer unchanged.
        let progress_layer = Self::layer(&mut self.layers, self.size, SensorMode::Progress);
        let region = Rect {
            x0: crop_rect.0,
            y0: crop_rect.1,
            x1: crop_rect.2,
            y1: crop_rect.3,
        };
        PanelRenderer::paste_image_region(progress_layer, pic, region, pos_x, pos_y);

        if self.save_processed_pic || self.save_progress_layer {
            // debugging only: materialize the masked graphic
            let mut processed_img = pic.clone();
            Self::apply_progress_mask(&mut processed_img, crop_rect, direction);
            let suffix = self.img_suffix.as_deref().unwrap_or_default();
            if self.save_processed_pic {
                let name = format!("processed_img-{}{suffix}.png", sensor.label);
                if let Err(e) = processed_img.save(self.img_save_path.join(name)) {
                    error!("Error saving processed image: {e}");
                }
            }
            if self.save_progress_layer {
                let name = format!("progress_layer-{}{suffix}.png", sensor.label);
                if let Err(e) = processed_img.save(self.img_save_path.join(name)) {
                    error!("Error saving progress layer image: {e}");
                }
            }
        }
        Ok(())
    }

    /// Mode 4 - Rotating pointer/dial indicator
    /// TODO needs testing
    fn render_pointer(
        &mut self,
        sensor: &Sensor,
        value: &str,
        direction: SensorDirection,
    ) -> Result<(), ImageProcessingError> {
        if !matches!(
            direction,
            SensorDirection::LeftToRight | SensorDirection::RightToLeft
        ) {
            return Err(ImageProcessingError::InvalidDirection(direction));
        }

        let x_center = sensor.x;
        let y_center = sensor.y;
        let xz_x = sensor.xz_x.unwrap_or(0);
        let xz_y = sensor.xz_y.unwrap_or(0);

        let pic_path = sensor.pic.as_ref().ok_or_else(|| {
            ImageProcessingError::ImageLoadError("No picture specified".to_string())
        })?;

        // Resize if dimensions specified
        let size = if let (Some(width), Some(height)) = (sensor.width, sensor.height) {
            Some((width, height))
        } else {
            None
        };
        let pic = self.image_cache.get(pic_path, size).ok_or_else(|| {
            ImageProcessingError::ImageLoadError(format!("Failed to load: {:?}", pic_path))
        })?;

        let min_val = sensor.min_value.unwrap_or(0.0);
        let max_val = sensor.max_value.unwrap_or(100.0);
        let current_value = value
            .parse::<f32>()
            .map_err(|_| ImageProcessingError::MathError("Invalid value".to_string()))?;

        let clamped_value = current_value.clamp(min_val, max_val);

        // Calculate progress
        let progress = if (max_val - min_val).abs() < f32::EPSILON {
            0.0
        } else {
            (clamped_value - min_val) / (max_val - min_val)
        };

        let mut min_angle = sensor.min_angle.unwrap_or(0) as f32;
        let mut max_angle = sensor.max_angle.unwrap_or(360) as f32;

        // Adjust angles for counter-clockwise
        if direction == SensorDirection::RightToLeft {
            min_angle = -min_angle;
            max_angle = -max_angle;
        }

        let angle = min_angle + progress * (max_angle - min_angle);
        let angle_rad = angle.to_radians();

        // Calculate offset based on rotation
        let offset_x = (xz_x as f32 * angle_rad.cos() - xz_y as f32 * angle_rad.sin()) as i32;
        let offset_y = (xz_x as f32 * angle_rad.sin() + xz_y as f32 * angle_rad.cos()) as i32;

        // Rotate the image (no copy needed for an unrotated pointer)
        let angle = angle.round() as i32;
        let rotated_pic = if angle == 0 {
            Cow::Borrowed(pic)
        } else {
            Cow::Owned(rotate_image(pic, -angle))
        };

        // Calculate final position
        let final_x = x_center + offset_x - (rotated_pic.width() / 2) as i32;
        let final_y = y_center + offset_y - (rotated_pic.height() / 2) as i32;

        let pointer_layer = Self::layer(&mut self.layers, self.size, SensorMode::Pointer);
        PanelRenderer::paste_image(pointer_layer, &rotated_pic, final_x, final_y);
        Ok(())
    }

    /// Draws a pie‐slice sector of the `source` image into the `layer` destination.
    ///
    /// Pixels in the sector are alpha-blended from source into the destination layer at the given
    /// center_x/center_y placement.
    ///
    /// Positive and negative angles are supported and are automatically normalized if > +/- 360°.
    ///
    /// # Arguments
    ///
    /// * `layer`: Destination layer.
    /// * `source`: Source image to cut out a pie-slice sector.
    /// * `center_x`: Center x position.
    /// * `center_y`: Center y position.
    /// * `start_deg`: Starting angle, in degrees. Angles are measured from 3 o’clock, increasing clockwise.
    /// * `end_deg`: Ending angle, in degrees.
    ///
    fn draw_pie_slice(
        layer: &mut RgbaImage,
        source: &RgbaImage,
        center_x: i32,
        center_y: i32,
        start_deg: f32,
        end_deg: f32,
    ) {
        let (src_w, src_h) = source.dimensions();
        // Radius is half the smaller dimension
        let radius = (src_w.min(src_h) as f32) / 2.0;
        // Convert angles to radians and normalize
        let start = (start_deg % 360f32).to_radians();
        let end = (end_deg % 360f32).to_radians();
        // Helper: check if angle t is between start and end (clockwise)
        let in_sector = |t: f32| {
            let mut a = t;
            if a < 0.0 {
                a += 2.0 * PI;
            }
            let mut s = start;
            let mut e = end;
            if s < 0.0 {
                s += 2.0 * PI;
            }
            if e < 0.0 {
                e += 2.0 * PI;
            }
            if e < s {
                // wrap
                a >= s || a <= e
            } else {
                a >= s && a <= e
            }
        };

        for sy in 0..src_h {
            for sx in 0..src_w {
                // Coordinates relative to center of source
                let dx = sx as f32 - src_w as f32 / 2.0;
                let dy = sy as f32 - src_h as f32 / 2.0;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist <= radius {
                    // Polar angle (atan2 returns [-PI, PI], 0 at +x axis)
                    let angle = dy.atan2(dx);
                    if in_sector(angle) {
                        // Pixel is inside the slice: blend it into layer
                        let dest_x = center_x + sx as i32 - src_w as i32 / 2;
                        let dest_y = center_y + sy as i32 - src_h as i32 / 2;
                        if dest_x >= 0 && dest_y >= 0 {
                            let (lw, lh) = layer.dimensions();
                            if (dest_x as u32) < lw && (dest_y as u32) < lh {
                                let src_px = source.get_pixel(sx, sy);
                                let dst_px = layer.get_pixel_mut(dest_x as u32, dest_y as u32);
                                // alpha‐blend: out = src.a*src + (1−src.a)*dst
                                let alpha = src_px[3] as f32 / 255.0;
                                for i in 0..4 {
                                    dst_px[i] = ((src_px[i] as f32 * alpha)
                                        + (dst_px[i] as f32 * (1.0 - alpha)))
                                        .round()
                                        as u8;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Apply progress mask to image based on crop rectangle and direction
    fn apply_progress_mask(
        image: &mut RgbaImage,
        crop_rect: (u32, u32, u32, u32),
        direction: SensorDirection,
    ) {
        let (crop_x, crop_y, crop_w, crop_h) = crop_rect;
        let (img_w, img_h) = image.dimensions();

        // Create mask - set alpha to 0 outside crop area
        for y in 0..img_h {
            for x in 0..img_w {
                let should_keep = match direction {
                    SensorDirection::LeftToRight => x < crop_w,
                    SensorDirection::RightToLeft => x >= crop_x,
                    SensorDirection::TopToBottom => y < crop_h,
                    SensorDirection::BottomToTop => y >= crop_y,
                };

                if !should_keep {
                    let pixel = image.get_pixel_mut(x, y);
                    pixel[3] = 0; // Set alpha to 0 (transparent)
                }
            }
        }
    }

    /// Paste an image onto a layer at specified position
    fn paste_image(target: &mut Layer, source: &RgbaImage, x: i32, y: i32) {
        let (source_w, source_h) = source.dimensions();
        let region = Rect {
            x0: 0,
            y0: 0,
            x1: source_w,
            y1: source_h,
        };
        PanelRenderer::paste_image_region(target, source, region, x, y);
    }

    /// Paste the `region` of `source` onto a layer, with the source origin at
    /// position (`x`, `y`): source pixel (sx, sy) lands on (x + sx, y + sy).
    fn paste_image_region(target: &mut Layer, source: &RgbaImage, region: Rect, x: i32, y: i32) {
        target.mark(
            x + region.x0 as i32,
            y + region.y0 as i32,
            region.x1.saturating_sub(region.x0),
            region.y1.saturating_sub(region.y0),
        );
        let target = &mut target.image;
        let (target_w, target_h) = target.dimensions();

        for sy in region.y0..region.y1 {
            for sx in region.x0..region.x1 {
                let target_x = x + sx as i32;
                let target_y = y + sy as i32;

                if target_x >= 0
                    && target_y >= 0
                    && (target_x as u32) < target_w
                    && (target_y as u32) < target_h
                {
                    let source_pixel = *source.get_pixel(sx, sy);
                    let target_pixel = target.get_pixel_mut(target_x as u32, target_y as u32);

                    // Alpha blending
                    let alpha = source_pixel[3] as f32 / 255.0;
                    let inv_alpha = 1.0 - alpha;

                    for i in 0..3 {
                        target_pixel[i] = ((source_pixel[i] as f32 * alpha)
                            + (target_pixel[i] as f32 * inv_alpha))
                            as u8;
                    }
                    target_pixel[3] = ((source_pixel[3] as f32 * alpha)
                        + (target_pixel[3] as f32 * inv_alpha))
                        as u8;
                }
            }
        }
    }

    fn create_img_save_path(&mut self) {
        if (self.save_render_img || self.save_processed_pic || self.save_progress_layer)
            && let Err(e) = fs::create_dir_all(&self.img_save_path)
        {
            error!(
                "Error creating image output path {:?}: {e}",
                self.img_save_path
            );
        }
    }

    /// The overlay layer of a graphic sensor mode, allocated on first use.
    ///
    /// Takes the layer array instead of `&mut self` so callers can hold a
    /// cached image from `image_cache` at the same time.
    fn layer(layers: &mut [Option<Layer>; 3], size: Size, mode: SensorMode) -> &mut Layer {
        let idx = LAYER_MODES
            .iter()
            .position(|m| *m == mode)
            .expect("graphic sensor mode");
        layers[idx].get_or_insert_with(|| Layer {
            image: RgbaImage::new(size.0, size.1),
            dirty: None,
        })
    }

    /// Composite all layers into final image
    fn composite_layers(&mut self, background: &mut RgbaImage) {
        for layer in self.layers.iter().flatten() {
            // Only the drawn area can contain non-transparent pixels
            if let Some(r) = layer.dirty {
                let layer = &layer.image;
                // Composite the layer onto final image
                for y in r.y0..r.y1 {
                    for x in r.x0..r.x1 {
                        let layer_pixel = *layer.get_pixel(x, y);
                        if layer_pixel[3] > 0 {
                            // If not fully transparent
                            let final_pixel = background.get_pixel_mut(x, y);

                            // Alpha compositing
                            let alpha = layer_pixel[3] as f32 / 255.0;
                            let inv_alpha = 1.0 - alpha;

                            for i in 0..4 {
                                final_pixel[i] = ((layer_pixel[i] as f32 * alpha)
                                    + (final_pixel[i] as f32 * inv_alpha))
                                    as u8;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The value a sensor displays: its value from `values`, or the current
/// date/time for `DATE_*` labels without a provided value.
fn resolved_sensor_value<'a>(
    sensor: &Sensor,
    values: &'a HashMap<String, String>,
    now: &DateTime<Local>,
) -> Option<Cow<'a, str>> {
    values
        .get(&sensor.label)
        .map(|v| Cow::Borrowed(v.as_str()))
        .or_else(|| get_date_time_value(&sensor.label, now).map(Cow::Owned))
}

/// The text a text sensor draws for `value`.
fn sensor_text(sensor: &Sensor, value: &str, unit: &str) -> String {
    format_value(
        value,
        sensor.integer_digits.into(),
        sensor.decimal_digits.unwrap_or_default() as usize,
        unit,
    )
}

/// Resolve the unit shown by a sensor. Provider metadata wins over the panel
/// declaration, matching the existing behavior for generic sensor sources.
/// Linux's built-in mapping points temperature widgets at value-only aliases,
/// so those aliases naturally fall back to the unit configured by Monitor3.
fn resolved_sensor_unit(sensor: &Sensor, values: &HashMap<String, String>) -> String {
    values
        .get(&format!("{}#unit", sensor.label))
        .cloned()
        .or_else(|| sensor.unit.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_only_linux_temperature_alias_does_not_append_provider_unit() {
        let sensor: Sensor = serde_json::from_str(
            r#"{"mode":1,"label":"temperature_cpu_value","value":"","x":0,"y":0,"unit":"","integerDigits":-1,"decimalDigits":0,"pic":""}"#,
        )
        .unwrap();
        let mut values = HashMap::new();
        values.insert("temperature_cpu_value".to_string(), "67.5".to_string());
        assert_eq!(resolved_sensor_unit(&sensor, &values), "");
        assert_eq!(
            format_value(
                "67.5",
                sensor.integer_digits.into(),
                sensor.decimal_digits.unwrap_or_default() as usize,
                &resolved_sensor_unit(&sensor, &values),
            ),
            "68"
        );
    }

    const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

    fn renderer() -> PanelRenderer {
        PanelRenderer::new(
            (960, 376),
            format!("{ASSETS}/fonts"),
            format!("{ASSETS}/cfg"),
        )
    }

    fn sensor_json(mode: u8, label: &str, x: i32, y: i32, extra: &str) -> String {
        format!(
            r#"{{"mode":{mode},"label":"{label}","x":{x},"y":{y},"value":"","unit":"","integerDigits":-1,"decimalDigits":0{extra}}}"#
        )
    }

    fn panel(sensors: &[String]) -> Panel {
        serde_json::from_str(&format!(
            r#"{{"img":"default_1_hdd.jpg","sensor":[{}]}}"#,
            sensors.join(",")
        ))
        .unwrap()
    }

    fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn text(label: &str) -> String {
        sensor_json(1, label, 100, 100, r#","fontSize":40,"pic":"""#)
    }

    fn progress(label: &str, x: i32, y: i32) -> String {
        sensor_json(3, label, x, y, r#","direction":1,"pic":"progress1.png""#)
    }

    #[test]
    fn unparseable_sensor_value_skips_only_that_sensor() {
        let full = panel(&[text("t"), progress("p", 50, 150)]);
        let text_only = panel(&[text("t")]);
        let vals = values(&[("t", "42"), ("p", "N/A")]);

        let mut r = renderer();
        let image = r.render(&full, &vals).unwrap().clone();
        assert_eq!(r.sensor_errors.len(), 1);
        let expected = renderer().render(&text_only, &vals).unwrap().clone();
        assert!(image == expected, "text sensor must still be rendered");

        // the sensor recovers once its value parses again
        r.render(&full, &values(&[("t", "42"), ("p", "50")]))
            .unwrap();
        assert!(r.sensor_errors.is_empty());
    }

    #[test]
    fn failing_sensor_sharing_a_label_is_logged_once() {
        // a text value and a progress bar on the same label: "N/A" only
        // breaks the bar, and the working text must not reset its state
        let p = panel(&[text("cpu"), progress("cpu", 50, 150)]);
        let mut r = renderer();
        let invalid = || Err(ImageProcessingError::MathError("Invalid value".into()));
        for frame in 0..3 {
            assert!(!r.track_sensor_result(&p.sensor[0], "N/A", Ok(())));
            let logged = r.track_sensor_result(&p.sensor[1], "N/A", invalid());
            assert_eq!(logged, frame == 0, "frame {frame}");
        }
        // full render path keeps exactly the one entry
        let vals = values(&[("cpu", "N/A")]);
        r.render(&p, &vals).unwrap();
        r.render(&p, &vals).unwrap();
        assert_eq!(r.sensor_errors.len(), 1);
        // recovery is logged once
        assert!(r.track_sensor_result(&p.sensor[1], "5", Ok(())));
        assert!(r.sensor_errors.is_empty());
    }

    #[test]
    fn reused_layers_match_a_fresh_renderer() {
        // high then low progress on the same renderer: the persistent layer
        // must be cleared exactly, compared to a fresh renderer
        let p = panel(&[progress("a", 50, 150), progress("b", -100, 300)]);
        let high = values(&[("a", "95"), ("b", "90")]);
        let low = values(&[("a", "10"), ("b", "60")]);
        let mut reused = renderer();
        reused.render(&p, &high).unwrap();
        let image = reused.render(&p, &low).unwrap().clone();
        let fresh = renderer().render(&p, &low).unwrap().clone();
        assert!(image == fresh);
        // and a panel without graphic sensors leaves no layer remnants
        let plain = panel(&[text("t")]);
        let vals = values(&[("t", "1")]);
        assert!(
            *reused.render(&plain, &vals).unwrap() == *renderer().render(&plain, &vals).unwrap()
        );
    }

    #[test]
    fn content_key_tracks_displayed_content() {
        let p = panel(&[text("t"), progress("p", 50, 150)]);
        let r = renderer();
        let now = Local::now();
        let key = |pairs: &[(&str, &str)]| r.content_key(&p, &values(pairs), &now);

        let base = key(&[("t", "42.1"), ("p", "50")]);
        assert_eq!(base, key(&[("t", "42.1"), ("p", "50"), ("other", "1")]));
        // decimalDigits 0: same displayed text, same key
        assert_eq!(base, key(&[("t", "42.3"), ("p", "50")]));
        assert_ne!(base, key(&[("t", "43"), ("p", "50")]));
        assert_ne!(base, key(&[("t", "42.1"), ("p", "51")]));
        assert_ne!(base, key(&[("t", "42.1")]), "missing value");
        assert_ne!(base, key(&[("t", "42.1"), ("t#unit", "%"), ("p", "50")]));
    }

    #[test]
    fn content_key_follows_the_clock_for_date_sensors() {
        use chrono::TimeZone;
        let p = panel(&[text("DATE_h_m_3")]);
        let r = renderer();
        let vals = HashMap::new();
        let t1 = Local.with_ymd_and_hms(2026, 11, 10, 12, 30, 5).unwrap();
        let t2 = Local.with_ymd_and_hms(2026, 11, 10, 12, 30, 55).unwrap();
        let t3 = Local.with_ymd_and_hms(2026, 11, 10, 12, 31, 0).unwrap();
        assert_eq!(r.content_key(&p, &vals, &t1), r.content_key(&p, &vals, &t2));
        assert_ne!(r.content_key(&p, &vals, &t1), r.content_key(&p, &vals, &t3));
    }

    #[test]
    fn configured_unit_remains_available_for_value_only_alias() {
        let sensor: Sensor = serde_json::from_str(
            r#"{"mode":1,"label":"temperature_memory_value","value":"","x":0,"y":0,"unit":" ℃","integerDigits":-1,"decimalDigits":0,"pic":""}"#,
        )
        .unwrap();
        let values = HashMap::new();
        assert_eq!(resolved_sensor_unit(&sensor, &values), " ℃");
    }

    #[test]
    fn pie_slice_blends_each_channel_exactly_once() {
        // Half-transparent orange over opaque blue.
        let mut source = RgbaImage::new(2, 2);
        for p in source.pixels_mut() {
            *p = Rgba([200, 100, 0, 128]);
        }
        let mut layer = RgbaImage::from_pixel(2, 2, Rgba([0, 0, 200, 255]));
        // Full circle so the pixel at the source center (1,1) is inside.
        PanelRenderer::draw_pie_slice(&mut layer, &source, 1, 1, 0.0, 359.9);

        let alpha = 128.0_f32 / 255.0;
        let blend = |s: f32, d: f32| (s * alpha + d * (1.0 - alpha)).round() as u8;
        let expected = Rgba([
            blend(200.0, 0.0),
            blend(100.0, 0.0),
            blend(0.0, 200.0),
            blend(128.0, 255.0),
        ]);
        assert_eq!(*layer.get_pixel(1, 1), expected);
    }
}
