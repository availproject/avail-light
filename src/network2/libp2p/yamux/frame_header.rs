pub enum FrameHeader {
    Data {
        stream_id: u32,
        length: u32,
        flags: Flags,
    },
    WindowUpdate {
        stream_id: u32,
        delta: u32,
        flags: Flags,
    },
    Ping {
        opaque_data: u32,
        outbound: bool,
    },
    GoAway(GoAwayCode),
}

impl FrameHeader {
    pub fn to_bytes(&self) -> [u8; 12] {
        let mut out = [0; 12];

        // Version
        out[0] = 0;

        // Type
        out[1] = match self {
            FrameHeader::Data { .. } => 0,
            FrameHeader::WindowUpdate { .. } => 1,
            FrameHeader::Ping { .. }=> 2,
            FrameHeader::GoAway(_) => 3,
        };

        // Flags
        out[2..4].copy_from_slice(&match self {
            FrameHeader::Data { flags, .. } => flags.to_u16(),
            FrameHeader::WindowUpdate { flags, .. } => flags.to_u16(),
            FrameHeader::Ping { outbound: true, .. } => 1,
            FrameHeader::Ping { outbound: false, .. } => 2,
            FrameHeader::GoAway(_) => 0,
        }.to_be_bytes()[..]);

        // Stream ID
        out[4..8].copy_from_slice(&match self {
            FrameHeader::Data { stream_id, .. } => *stream_id,
            FrameHeader::WindowUpdate { stream_id, .. } => *stream_id,
            FrameHeader::Ping { .. } => 0,
            FrameHeader::GoAway(_) => 0,
        }.to_be_bytes()[..]);

        // Length
        out[8..12].copy_from_slice(&match self {
            FrameHeader::Data { length, .. } => *length,
            FrameHeader::WindowUpdate { delta, .. } => *delta,
            FrameHeader::Ping { opaque_data, .. } => *opaque_data,
            FrameHeader::GoAway(code) => match code {
                GoAwayCode::NormalTermination => 0,
                GoAwayCode::ProtocolError => 1,
                GoAwayCode::InternalError => 2,
            },
        }.to_be_bytes()[..]);

        out
    }

    pub fn from_bytes(bytes: &[u8; 12]) -> Self {
        todo!()
    }
}

pub struct Flags {
    syn: bool,
    ack: bool,
    fin: bool,
    rst: bool,
}

impl Flags {
    fn to_u16(&self) -> u16 {
        let mut out = 0u16;
        if self.syn { out |= 1 };
        if self.ack { out |= 2 };
        if self.fin { out |= 4 };
        if self.rst { out |= 8 };
        out
    }
}

pub enum GoAwayCode {
    NormalTermination,
    ProtocolError,
    InternalError,
}
