mod frame_header;

enum State {
    HeaderWait,
    FrameWait { remaining_len: u32 },
}
