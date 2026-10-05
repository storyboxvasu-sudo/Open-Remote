use app_common::FrameMeta;
use core_capture::RawFrame;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CodecError {
    #[error("Encoding error: {0}")]
    EncodingFailed(String),
    #[error("Decoding error: {0}")]
    DecodingFailed(String),
}

pub struct CompressedFrame {
    pub meta: FrameMeta,
    pub payload: Vec<u8>,
}

pub struct FrameEncoder {
    prev_frame: Mutex<Option<Vec<u8>>>,
    prev_width: AtomicU32,
    prev_height: AtomicU32,
    frame_counter: AtomicU64,
}

impl Default for FrameEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameEncoder {
    pub fn new() -> Self {
        Self {
            prev_frame: Mutex::new(None),
            prev_width: AtomicU32::new(0),
            prev_height: AtomicU32::new(0),
            frame_counter: AtomicU64::new(0),
        }
    }

    /// Resets cached previous frame state (e.g. on monitor switch)
    pub fn reset(&self) {
        *self.prev_frame.lock() = None;
        self.prev_width.store(0, Ordering::Relaxed);
        self.prev_height.store(0, Ordering::Relaxed);
        self.frame_counter.store(0, Ordering::Relaxed);
    }

    /// Encodes a frame with dirty-rect delta compression.
    /// Returns Ok(None) if the frame is completely unchanged from the previous frame.
    pub fn encode(&self, frame: &RawFrame) -> Result<Option<CompressedFrame>, CodecError> {
        let width = frame.width;
        let height = frame.height;
        let row_bytes = (width * 4) as usize;
        let total_bytes = row_bytes * (height as usize);

        if frame.data.len() != total_bytes {
            return Err(CodecError::EncodingFailed(format!(
                "Invalid frame buffer size: expected {}, got {}",
                total_bytes,
                frame.data.len()
            )));
        }

        let mut prev_guard = self.prev_frame.lock();
        let prev_w = self.prev_width.load(Ordering::Relaxed);
        let prev_h = self.prev_height.load(Ordering::Relaxed);
        let count = self.frame_counter.fetch_add(1, Ordering::Relaxed);

        // Send a full keyframe if first frame, dimensions changed, or every 60 frames (~1-2 seconds of activity)
        let is_keyframe = prev_guard.is_none() || prev_w != width || prev_h != height || (count % 60 == 0);

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        if is_keyframe {
            let compressed = lz4_flex::compress_prepend_size(&frame.data);
            *prev_guard = Some(frame.data.clone());
            self.prev_width.store(width, Ordering::Relaxed);
            self.prev_height.store(height, Ordering::Relaxed);

            return Ok(Some(CompressedFrame {
                meta: FrameMeta {
                    width,
                    height,
                    timestamp_ms,
                    is_keyframe: true,
                    dirty_x: 0,
                    dirty_y: 0,
                    dirty_w: width,
                    dirty_h: height,
                    monitors: None,
                    active_monitor: None,
                    access_level: None,
                },
                payload: compressed,
            }));
        }

        let prev = prev_guard.as_mut().unwrap();

        // Scan for changed rows using fast slice comparison
        let mut min_y = usize::MAX;
        let mut max_y = 0;

        for y in 0..height as usize {
            let start = y * row_bytes;
            let end = start + row_bytes;
            if prev[start..end] != frame.data[start..end] {
                if min_y == usize::MAX {
                    min_y = y;
                }
                max_y = y;
            }
        }

        // Screen is identical to previous frame: skip transmission completely!
        if min_y > max_y {
            return Ok(None);
        }

        let dirty_y = min_y as u32;
        let dirty_h = (max_y - min_y + 1) as u32;
        let dirty_x = 0;
        let dirty_w = width;

        let start_byte = min_y * row_bytes;
        let end_byte = (max_y + 1) * row_bytes;
        let dirty_slice = &frame.data[start_byte..end_byte];

        // Update cached previous frame
        prev[start_byte..end_byte].copy_from_slice(dirty_slice);

        // Compress ONLY the changed region
        let compressed = lz4_flex::compress_prepend_size(dirty_slice);

        Ok(Some(CompressedFrame {
            meta: FrameMeta {
                width,
                height,
                timestamp_ms,
                is_keyframe: false,
                dirty_x,
                dirty_y,
                dirty_w,
                dirty_h,
                monitors: None,
                active_monitor: None,
                access_level: None,
            },
            payload: compressed,
        }))
    }

    /// Decompresses an LZ4 payload back into raw 32-bit pixel buffer
    pub fn decode(&self, payload: &[u8]) -> Result<Vec<u8>, CodecError> {
        lz4_flex::decompress_size_prepended(payload)
            .map_err(|e| CodecError::DecodingFailed(e.to_string()))
    }
}
