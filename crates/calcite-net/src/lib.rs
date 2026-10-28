use bytes::BytesMut;
use calcite_protocol::{ProtocolError, decode_frame};

pub struct FrameDecoder {
    buffer: BytesMut,
    max_frame: usize,
}

impl FrameDecoder {
    pub fn new(max_frame: usize) -> Self {
        Self {
            buffer: BytesMut::new(),
            max_frame,
        }
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<BytesMut>, ProtocolError> {
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some(frame) = decode_frame(&mut self.buffer)? {
            if frame.len() > self.max_frame {
                return Err(ProtocolError::PacketTooLarge);
            }
            frames.push(frame);
        }
        Ok(frames)
    }
}
