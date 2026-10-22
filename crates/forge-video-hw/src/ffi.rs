//! The little that is needed around the raw bindings: errors as text,
//! owned frames and packets, and C strings.

use ffmpeg_sys_next as ff;
use forge_video::codec::CodecError;
use std::ffi::{CStr, CString};
use std::os::raw::c_int;

/// FFmpeg's error code as its message.
pub fn av_error(what: &str, code: c_int) -> CodecError {
    let mut buf = [0i8; 128];
    let msg = unsafe {
        ff::av_strerror(code, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    };
    CodecError::Codec(format!("{what}: {msg} ({code})"))
}

pub fn check(what: &str, code: c_int) -> Result<(), CodecError> {
    if code < 0 {
        Err(av_error(what, code))
    } else {
        Ok(())
    }
}

pub fn cstr(s: &str) -> CString {
    CString::new(s).unwrap_or_else(|_| CString::new("?").expect("valid"))
}

/// An owned `AVFrame`.
pub struct Frame(pub *mut ff::AVFrame);

// An AVFrame's buffers are reference counted and its device memory is
// not thread-affine; FFmpeg's CUDA contexts are pushed per call.
unsafe impl Send for Frame {}
unsafe impl Sync for Frame {}

impl Frame {
    pub fn new() -> Result<Frame, CodecError> {
        let p = unsafe { ff::av_frame_alloc() };
        if p.is_null() {
            return Err(CodecError::Codec("av_frame_alloc failed".into()));
        }
        Ok(Frame(p))
    }

    /// Another reference to the same buffers.
    pub fn clone_ref(&self) -> Result<Frame, CodecError> {
        let f = Frame::new()?;
        check("av_frame_ref", unsafe { ff::av_frame_ref(f.0, self.0) })?;
        Ok(f)
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { ff::av_frame_free(&mut self.0) };
    }
}

/// An owned `AVPacket`.
pub struct Packet(pub *mut ff::AVPacket);

unsafe impl Send for Packet {}

impl Packet {
    pub fn new() -> Result<Packet, CodecError> {
        let p = unsafe { ff::av_packet_alloc() };
        if p.is_null() {
            return Err(CodecError::Codec("av_packet_alloc failed".into()));
        }
        Ok(Packet(p))
    }

    /// A packet holding a copy of `data` (padded as FFmpeg wants).
    pub fn from_data(data: &[u8]) -> Result<Packet, CodecError> {
        let p = Packet::new()?;
        check("av_new_packet", unsafe {
            ff::av_new_packet(p.0, data.len() as c_int)
        })?;
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), (*p.0).data, data.len());
        }
        Ok(p)
    }

    pub fn data(&self) -> &[u8] {
        unsafe {
            let p = &*self.0;
            if p.data.is_null() || p.size <= 0 {
                &[]
            } else {
                std::slice::from_raw_parts(p.data, p.size as usize)
            }
        }
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        unsafe { ff::av_packet_free(&mut self.0) };
    }
}

/// An owned `AVBufferRef` (a device or frames context).
pub struct BufferRef(pub *mut ff::AVBufferRef);

unsafe impl Send for BufferRef {}
unsafe impl Sync for BufferRef {}

impl BufferRef {
    pub fn clone_ref(&self) -> BufferRef {
        BufferRef(unsafe { ff::av_buffer_ref(self.0) })
    }

    /// Give the reference to FFmpeg (a context field that will unref it).
    pub fn into_raw(self) -> *mut ff::AVBufferRef {
        let p = self.0;
        std::mem::forget(self);
        p
    }
}

impl Drop for BufferRef {
    fn drop(&mut self) {
        unsafe { ff::av_buffer_unref(&mut self.0) };
    }
}

/// An owned `AVCodecContext`.
pub struct CodecContext(pub *mut ff::AVCodecContext);

unsafe impl Send for CodecContext {}

impl Drop for CodecContext {
    fn drop(&mut self) {
        unsafe { ff::avcodec_free_context(&mut self.0) };
    }
}
