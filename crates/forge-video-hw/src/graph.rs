//! An FFmpeg filter graph over device frames, as the scaler and the
//! compositor build one: buffer sources described by the frames they
//! will take, filters by name and arguments, one sink.

use crate::ffi::{self, Frame};
use ffmpeg_sys_next as ff;
use forge_video::codec::CodecError;
use forge_video::frame::Resolution;
use std::os::raw::c_int;

/// A graph under construction or in use. Inputs are numbered in the
/// order they were added.
pub struct FilterGraph {
    graph: *mut ff::AVFilterGraph,
    inputs: Vec<*mut ff::AVFilterContext>,
    sink: *mut ff::AVFilterContext,
    filters: usize,
}

// The graph is used from one thread at a time (behind the stage that
// owns it); its device memory is not thread-affine.
unsafe impl Send for FilterGraph {}

impl Drop for FilterGraph {
    fn drop(&mut self) {
        unsafe { ff::avfilter_graph_free(&mut self.graph) };
    }
}

/// A filter in a graph, to link from and to.
#[derive(Clone, Copy)]
pub struct Node(*mut ff::AVFilterContext);

impl FilterGraph {
    pub fn new() -> Result<FilterGraph, CodecError> {
        let graph = unsafe { ff::avfilter_graph_alloc() };
        if graph.is_null() {
            return Err(CodecError::Codec("avfilter_graph_alloc failed".into()));
        }
        Ok(FilterGraph {
            graph,
            inputs: Vec::new(),
            sink: std::ptr::null_mut(),
            filters: 0,
        })
    }

    fn alloc(&mut self, filter: &str, label: &str) -> Result<*mut ff::AVFilterContext, CodecError> {
        let f = unsafe { ff::avfilter_get_by_name(ffi::cstr(filter).as_ptr()) };
        if f.is_null() {
            return Err(CodecError::Codec(format!(
                "libavfilter has no {filter} filter"
            )));
        }
        let ctx =
            unsafe { ff::avfilter_graph_alloc_filter(self.graph, f, ffi::cstr(label).as_ptr()) };
        if ctx.is_null() {
            return Err(CodecError::Codec(format!(
                "could not allocate the {filter} filter"
            )));
        }
        Ok(ctx)
    }

    /// A buffer source taking frames like `like` (its pixel format and
    /// device frames context) of `resolution`. Returns the input's index
    /// and its node.
    pub fn add_source(
        &mut self,
        like: *mut ff::AVFrame,
        resolution: Resolution,
    ) -> Result<(usize, Node), CodecError> {
        let idx = self.inputs.len();
        let src = self.alloc("buffer", &format!("in{idx}"))?;
        unsafe {
            let par = ff::av_buffersrc_parameters_alloc();
            (*par).format = (*like).format;
            (*par).width = resolution.width as i32;
            (*par).height = resolution.height as i32;
            (*par).time_base = ff::AVRational {
                num: 1,
                den: 90_000,
            };
            (*par).hw_frames_ctx = ff::av_buffer_ref((*like).hw_frames_ctx);
            let rc = ff::av_buffersrc_parameters_set(src, par);
            ff::av_buffer_unref(&mut (*par).hw_frames_ctx);
            ff::av_free(par as *mut _);
            ffi::check("av_buffersrc_parameters_set", rc)?;
            ffi::check("init buffer", ff::avfilter_init_str(src, std::ptr::null()))?;
        }
        self.inputs.push(src);
        Ok((idx, Node(src)))
    }

    /// A filter by name with its argument string.
    pub fn add_filter(&mut self, filter: &str, args: &str) -> Result<Node, CodecError> {
        self.filters += 1;
        let ctx = self.alloc(filter, &format!("{filter}{}", self.filters))?;
        let a = ffi::cstr(args);
        ffi::check(&format!("init {filter}={args}"), unsafe {
            ff::avfilter_init_str(ctx, a.as_ptr())
        })?;
        Ok(Node(ctx))
    }

    /// The graph's one output, fed by `from`.
    pub fn add_sink(&mut self, from: Node) -> Result<(), CodecError> {
        let sink = self.alloc("buffersink", "out")?;
        ffi::check("init buffersink", unsafe {
            ff::avfilter_init_str(sink, std::ptr::null())
        })?;
        self.sink = sink;
        self.link(from, 0, Node(sink), 0)
    }

    pub fn link(&mut self, from: Node, out: u32, to: Node, inp: u32) -> Result<(), CodecError> {
        ffi::check("avfilter_link", unsafe {
            ff::avfilter_link(from.0, out, to.0, inp)
        })
    }

    pub fn configure(&mut self) -> Result<(), CodecError> {
        if self.sink.is_null() {
            return Err(CodecError::Codec("filter graph without a sink".into()));
        }
        ffi::check("avfilter_graph_config", unsafe {
            ff::avfilter_graph_config(self.graph, std::ptr::null_mut())
        })
    }

    /// Feed input `idx` a reference to `frame`, stamped `pts`.
    pub fn push(
        &mut self,
        idx: usize,
        frame: *mut ff::AVFrame,
        pts: i64,
    ) -> Result<(), CodecError> {
        let src = *self
            .inputs
            .get(idx)
            .ok_or_else(|| CodecError::Codec(format!("filter graph has no input {idx}")))?;
        let input = Frame::new()?;
        ffi::check("av_frame_ref", unsafe { ff::av_frame_ref(input.0, frame) })?;
        unsafe {
            (*input.0).pts = pts;
        }
        ffi::check("av_buffersrc_add_frame", unsafe {
            ff::av_buffersrc_add_frame_flags(src, input.0, ff::AV_BUFFERSRC_FLAG_KEEP_REF as c_int)
        })
    }

    /// One frame from the sink.
    pub fn pull(&mut self) -> Result<Frame, CodecError> {
        let out = Frame::new()?;
        ffi::check("av_buffersink_get_frame", unsafe {
            ff::av_buffersink_get_frame(self.sink, out.0)
        })?;
        Ok(out)
    }
}
