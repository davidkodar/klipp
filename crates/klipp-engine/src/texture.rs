use std::sync::{Arc, Mutex};

pub struct FrameBuffer {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub dirty: bool,
}

impl FrameBuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            data: vec![0u8; (width * height * 4) as usize],
            width,
            height,
            dirty: false,
        }
    }

    pub fn write_frame(&mut self, rgba_data: Vec<u8>, width: u32, height: u32) {
        self.data = rgba_data;
        self.width = width;
        self.height = height;
        self.dirty = true;
    }

    pub fn clear(&mut self) {
        self.data.fill(0);
        self.dirty = true;
    }
}

pub type SharedFrameBuffer = Arc<Mutex<FrameBuffer>>;
