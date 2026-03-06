use serde::{Deserialize, Serialize};

/// Stream/session close reasons.
/// Mapped from include/tbl/sess_close.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StreamClose {
    /// Peer closed the connection
    RemClose,
    /// Peer requested close (Connection: close)
    ReqClose,
    /// Protocol version < HTTP/1.1
    ReqHttp10,
    /// Received bad request/response
    RxBad,
    /// Failure receiving body
    RxBody,
    /// Received junk data
    RxJunk,
    /// Received buffer overflow
    RxOverflow,
    /// Receive timeout
    RxTimeout,
    /// timeout_idle reached
    RxCloseIdle,
    /// Piped transaction
    TxPipe,
    /// Error in transaction
    TxError,
    /// EOF transmission
    TxEof,
    /// Backend/VCL requested close
    RespClose,
    /// Out of some resource
    Overload,
    /// Session pipe overflow
    PipeOverflow,
    /// Insufficient data for range
    RangeShort,
    /// HTTP2 not accepted
    ReqHttp20,
    /// VCL failure
    VclFailure,
    /// HTTP2 rapid reset
    RapidReset,
    /// HTTP2 credit bankruptcy
    Bankrupt,
}

impl StreamClose {
    pub fn name(&self) -> &'static str {
        match self {
            Self::RemClose => "rem_close",
            Self::ReqClose => "req_close",
            Self::ReqHttp10 => "req_http10",
            Self::RxBad => "rx_bad",
            Self::RxBody => "rx_body",
            Self::RxJunk => "rx_junk",
            Self::RxOverflow => "rx_overflow",
            Self::RxTimeout => "rx_timeout",
            Self::RxCloseIdle => "rx_close_idle",
            Self::TxPipe => "tx_pipe",
            Self::TxError => "tx_error",
            Self::TxEof => "tx_eof",
            Self::RespClose => "resp_close",
            Self::Overload => "overload",
            Self::PipeOverflow => "pipe_overflow",
            Self::RangeShort => "range_short",
            Self::ReqHttp20 => "req_http20",
            Self::VclFailure => "vcl_failure",
            Self::RapidReset => "rapid_reset",
            Self::Bankrupt => "bankrupt",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::RemClose => "Peer Closed",
            Self::ReqClose => "Peer requested close",
            Self::ReqHttp10 => "Proto < HTTP/1.1",
            Self::RxBad => "Received bad req/resp",
            Self::RxBody => "Failure receiving body",
            Self::RxJunk => "Received junk data",
            Self::RxOverflow => "Received buffer overflow",
            Self::RxTimeout => "Receive timeout",
            Self::RxCloseIdle => "timeout_idle reached",
            Self::TxPipe => "Piped transaction",
            Self::TxError => "Error transaction",
            Self::TxEof => "EOF transmission",
            Self::RespClose => "Backend/VCL requested close",
            Self::Overload => "Out of some resource",
            Self::PipeOverflow => "Session pipe overflow",
            Self::RangeShort => "Insufficient data for range",
            Self::ReqHttp20 => "HTTP2 not accepted",
            Self::VclFailure => "VCL failure",
            Self::RapidReset => "HTTP2 rapid reset",
            Self::Bankrupt => "HTTP2 credit bankruptcy",
        }
    }

    /// Whether this close reason represents an error condition.
    pub fn is_error(&self) -> bool {
        !matches!(
            self,
            Self::RemClose
                | Self::ReqClose
                | Self::RxCloseIdle
                | Self::TxPipe
                | Self::TxEof
                | Self::RespClose
        )
    }
}

impl std::fmt::Display for StreamClose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}
