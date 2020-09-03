mod frame_header;

pub struct SubstreamState {}

enum State {
    HeaderWait,
    FrameWait { remaining_len: u32 },
}
