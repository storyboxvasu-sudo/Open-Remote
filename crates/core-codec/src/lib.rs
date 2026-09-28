use app_common::FrameMeta;
use core_capture::RawFrame;
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

pub struct FrameEncoder;

impl FrameEncoder {
    pub fn new() -> Self {
        Self
    }

    /// Compresses raw 32-bit screen bitmap into an ultra-low latency LZ4 payload (<2ms encode)
    pub fn encode(&self, frame: &RawFrame) -> Result<CompressedFrame, CodecError> {
        let compressed = lz4_flex::compress_prepend_size(&frame.data);

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Ok(CompressedFrame {
            meta: FrameMeta {
                width: frame.width,
                height: frame.height,
                timestamp_ms,
                is_keyframe: true,
            },
            payload: compressed,
        })
    }

    /// Decompresses an LZ4 payload back into raw 32-bit RGBA pixel buffer
    pub fn decode(&self, payload: &[u8]) -> Result<Vec<u8>, CodecError> {
        lz4_flex::decompress_size_prepended(payload)
            .map_err(|e| CodecError::DecodingFailed(e.to_string()))
    }
}
