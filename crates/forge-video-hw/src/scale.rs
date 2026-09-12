//! A scaler on the device: FFmpeg's `scale_cuda` (or the device's own
//! filter) in a two-node graph, one graph per pair of sizes, kept.

use crate::device::HwDevice;
use crate::frame::{hw_frame, wrap};
use crate::graph::FilterGraph;
use ffmpeg_sys_next as ff;
use forge_video::codec::CodecError;
use forge_video::frame::{MediaDevice, Resolution, VideoFrame};
use forge_video::scale::{ScaleMode, Scaler};
use std::sync::Arc;

struct Graph {
    from: Resolution,
    to: Resolution,
    mode: ScaleMode,
    graph: FilterGraph,
    ticks: i64,
}

/// The scale filter a device kind has.
pub(crate) fn filter_for(kind: ff::AVHWDeviceType) -> &'static str {
    match kind {
        ff::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA => "scale_cuda",
        ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI => "scale_vaapi",
        ff::AVHWDeviceType::AV_HWDEVICE_TYPE_QSV => "scale_qsv",
        _ => "scale",
    }
}

/// The scale filter's arguments for a resampling: bilinear matches the
/// host's camera path; a shrink of screen content takes Lanczos, the
/// nearest the device has to the host's box filter for keeping text.
pub(crate) fn scale_args(kind: ff::AVHWDeviceType, to: Resolution, mode: ScaleMode) -> String {
    let algo = match (kind, mode) {
        (ff::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA, ScaleMode::Bilinear) => ":interp_algo=bilinear",
        (ff::AVHWDeviceType::AV_HWDEVICE_TYPE_CUDA, ScaleMode::Box) => ":interp_algo=lanczos",
        _ => "",
    };
    format!("w={}:h={}{algo}", to.width, to.height)
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
        mode: ScaleMode,
    ) -> Result<Graph, CodecError> {
        let mut graph = FilterGraph::new()?;
        let (_, input) = graph.add_source(src_frame, from)?;
        let kind = self.device.kind();
        let scale = graph.add_filter(filter_for(kind), &scale_args(kind, to, mode))?;
        graph.link(input, 0, scale, 0)?;
        graph.add_sink(scale)?;
        graph.configure()?;
        Ok(Graph {
            from,
            to,
            mode,
            graph,
            ticks: 0,
        })
    }

    /// [`Scaler::scale`] with a choice of resampling.
    pub fn scale_with(
        &mut self,
        src: &VideoFrame,
        to: Resolution,
        mode: ScaleMode,
    ) -> Result<VideoFrame, CodecError> {
        let hw = hw_frame(src, self.device.device())?;
        let from = Resolution::new(hw.width(), hw.height());
        let pts = src.pts();
        if from == to {
            return Ok(src.clone());
        }
        let idx = match self
            .graphs
            .iter()
            .position(|g| g.from == from && g.to == to && g.mode == mode)
        {
            Some(i) => i,
            None => {
                // A room has a handful of sizes; a bound keeps a churn of
                // sizes from growing the list forever.
                if self.graphs.len() >= 16 {
                    self.graphs.remove(0);
                }
                let g = self.build(hw.raw(), from, to, mode)?;
                self.graphs.push(g);
                self.graphs.len() - 1
            }
        };
        let g = &mut self.graphs[idx];
        // The graph's own clock: a frame per call, whatever the callers'
        // timestamps do.
        g.ticks += 1;
        g.graph.push(0, hw.raw(), g.ticks)?;
        let out = g.graph.pull()?;
        Ok(wrap(self.device.device(), out, pts))
    }
}

impl Scaler for DeviceScaler {
    fn device(&self) -> MediaDevice {
        self.device.device().clone()
    }

    fn scale(&mut self, src: &VideoFrame, to: Resolution) -> Result<VideoFrame, CodecError> {
        self.scale_with(src, to, ScaleMode::Bilinear)
    }
}
