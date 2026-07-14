use hbb_common::{
    bail,
    message_proto::{message, video_frame, Message},
    ResultType,
};
use std::sync::mpsc::Sender;

#[derive(Debug, Clone)]
pub struct RecorderContext {
    pub server: bool,
    pub id: String,
    pub dir: String,
    pub display_idx: usize,
    pub camera: bool,
    pub tx: Option<Sender<RecordState>>,
}

#[derive(Debug)]
pub enum RecordState {
    NewFile(String),
    NewFrame,
    WriteTail,
    RemoveFile,
}

pub struct Recorder {
    _ctx: RecorderContext,
}

impl Recorder {
    pub fn new(_ctx: RecorderContext) -> ResultType<Self> {
        bail!("Screen recording is not available on OHOS")
    }

    pub fn write_message(&mut self, msg: &Message, w: usize, h: usize) {
        if let Some(message::Union::VideoFrame(vf)) = &msg.union {
            if let Some(frame) = &vf.union {
                let _ = self.write_frame(frame, w, h);
            }
        }
    }

    pub fn write_frame(
        &mut self,
        _frame: &video_frame::Union,
        _w: usize,
        _h: usize,
    ) -> ResultType<()> {
        bail!("Screen recording is not available on OHOS")
    }
}
