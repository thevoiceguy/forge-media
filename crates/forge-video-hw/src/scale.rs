//! A scaler on the device: FFmpeg's `scale_cuda` (or the device's own
//! filter) in a two-node graph, one graph per pair of sizes, kept.

use crate::device::HwDevice;
use crate::ffi::{self, Frame};
use crate::frame::{hw_frame, wrap};
use ffmpeg_sys_next as ff;
use forge_video::codec::CodecError;
use forge_video::frame::{MediaDevice, Resolution, VideoFrame};
use forge_video::scale::Scaler;
use std::sync::Arc;

struct Graph {
    from: Resolution,
    to: Resolution,
    graph: *mut ff::AVFilterGraph,
    src: *mut ff::AVFilterContext,
    sink: *mut ff::AVFilterContext,
}

unsafe impl Send for Graph {}

impl Drop for Graph {
    fn drop(&mut self) {
        unsafe { ff::avfilter_graph_free(&mut self.graph) };
    }
}

/// The scale filter a device kind has.
fn filter_for(kind: ff::AVHWDeviceType) -> &'static str {
    match kind {
        ff::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA => "scale_cuda",
        ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI => "scale_vaapi",
        ff::AVHWDeviceType::AV_HWDEVICE_TYPE_QSV => "scale_qsv",
        _ => "scale",
    }
}

/// Scales device frames to any size, on the device.
pub struct DeviceScaler {
    device: Arc<HwDevice>,
    graphs: Vec<Graph>,
}

impl DeviceScaler {
    pub fn new(device: Arc<HwDevice>) -> DeviceScaler {
        DeviceScaler {
            device,
            graphs: Vec::new(),
        }
    }

    fn build(
        &self,
        src_frame: *mut ff::AVFrame,
        from: Resolution,
        to: Resolution,
    ) -> Result<Graph, CodecError> {
        unsafe {
            let graph = ff::avfilter_graph_alloc();
            if graph.is_null() {
                return Err(CodecError::Codec("avfilter_graph_alloc failed".into()));
            }
            let mut g = Graph {
                from,
                to,
                graph,
                src: std::ptr::null_mut(),
                sink: std::ptr::null_mut(),
            };
            // The source is described by the frame itself: its device
            // frames context tells the graph where the pixels are.
            let buffer = ff::avfilter_get_by_name(c"buffer".as_ptr());
            let buffersink = ff::avfilter_get_by_name(c"buffersink".as_ptr());
            if buffer.is_null() || buffersink.is_null() {
                return Err(CodecError::Codec(
                    "libavfilter has no buffer filters".into(),
                ));
            }
            g.src = ff::avfilter_graph_alloc_filter(graph, buffer, c"in".as_ptr());
            g.sink = ff::avfilter_graph_alloc_filter(graph, buffersink, c"out".as_ptr());
            if g.src.is_null() || g.sink.is_null() {
                return Err(CodecError::Codec(
                    "avfilter_graph_alloc_filter failed".into(),
                ));
            }
            let par = ff::av_buffersrc_parameters_alloc();
            (*par).format = (*src_frame).format;
            (*par).width = from.width as i32;
            (*par).height = from.height as i32;
            (*par).time_base = ff::AVRational {
                num: 1,
                den: 90_000,
            };
            (*par).hw_frames_ctx = ff::av_buffer_ref((*src_frame).hw_frames_ctx);
            let rc = ff::av_buffersrc_parameters_set(g.src, par);
            ff::av_buffer_unref(&mut (*par).hw_frames_ctx);
            ff::av_free(par as *mut _);
            ffi::check("av_buffersrc_parameters_set", rc)?;
            ffi::check(
                "init buffer",
                ff::avfilter_init_str(g.src, std::ptr::null()),
            )?;
            ffi::check(
                "init buffersink",
                ff::avfilter_init_str(g.sink, std::ptr::null()),
            )?;

            let scale =
                ff::avfilter_get_by_name(ffi::cstr(filter_for(self.device.kind())).as_ptr());
            if scale.is_null() {
                return Err(CodecError::Unavailable {
                    codec: forge_core::VideoCodec::H264,
                    role: filter_for(self.device.kind()),
                    device: self.device.device().clone(),
                });
            }
            let sc = ff::avfilter_graph_alloc_filter(graph, scale, c"scale".as_ptr());
            if sc.is_null() {
                return Err(CodecError::Codec(
                    "could not allocate the scale filter".into(),
                ));
            }
            let args = ffi::cstr(&format!("w={}:h={}", to.width, to.height));
            ffi::check("init scale", ff::avfilter_init_str(sc, args.as_ptr()))?;
            ffi::check("link in→scale", ff::avfilter_link(g.src, 0, sc, 0))?;
            ffi::check("link scale→out", ff::avfilter_link(sc, 0, g.sink, 0))?;
            ffi::check(
                "avfilter_graph_config",
                ff::avfilter_graph_config(graph, std::ptr::null_mut()),
            )?;
            Ok(g)
        }
    }
}

impl Scaler for DeviceScaler {
    fn device(&self) -> MediaDevice {
        self.device.device().clone()
    }

    fn scale(&mut self, src: &VideoFrame, to: Resolution) -> Result<VideoFrame, CodecError> {
        let hw = hw_frame(src, self.device.device())?;
        let from = Resolution::new(hw.width(), hw.height());
        let pts = match src {
            VideoFrame::Device(d) => d.pts,
            VideoFrame::Host(h) => h.pts,
        };
        if from == to {
            return Ok(src.clone());
        }
        let idx = match self
            .graphs
            .iter()
            .position(|g| g.from == from && g.to == to)
        {
            Some(i) => i,
            None => {
                let g = self.build(hw.raw(), from, to)?;
                self.graphs.push(g);
                // A room has a handful of sizes; a bound keeps a churn of
                // sizes from growing the list forever.
                if self.graphs.len() > 16 {
                    self.graphs.remove(0);
                    self.graphs.len() - 1
                } else {
                    self.graphs.len() - 1
                }
            }
        };
        let g = &self.graphs[idx];
        let input = hw.frame.clone_ref()?;
        unsafe {
            (*input.0).pts = pts as i64;
        }
        ffi::check("av_buffersrc_add_frame", unsafe {
            ff::av_buffersrc_add_frame_flags(g.src, input.0, ff::AV_BUFFERSRC_FLAG_KEEP_REF as i32)
        })?;
        let out = Frame::new()?;
        ffi::check("av_buffersink_get_frame", unsafe {
            ff::av_buffersink_get_frame(g.sink, out.0)
        })?;
        Ok(wrap(self.device.device(), out, pts))
    }
}
