use serde::{Deserialize, Serialize};

/// Header filter flags from include/tbl/http_headers.h.
/// P = filter on pass, F = filter on fetch, I = filter on insert,
/// S = filter on pass (synonymous with P), K = connection-specific
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct HeaderFlags(u8);

impl HeaderFlags {
    pub const PASS: Self = Self(1 << 0);
    pub const FETCH: Self = Self(1 << 1);
    pub const INSERT: Self = Self(1 << 2);
    pub const PASS_S: Self = Self(1 << 3);
    pub const CONNECTION: Self = Self(1 << 4);

    pub fn empty() -> Self {
        Self(0)
    }

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for HeaderFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Well-known HTTP headers.
/// Mapped from include/tbl/http_headers.h
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KnownHeader {
    Accept,
    AcceptCharset,
    AcceptEncoding,
    AcceptLanguage,
    AcceptRanges,
    Age,
    Allow,
    Authorization,
    CacheControl,
    Connection,
    ContentEncoding,
    ContentLanguage,
    ContentLength,
    ContentLocation,
    ContentMd5,
    ContentRange,
    ContentType,
    Cookie,
    Date,
    ETag,
    Expect,
    Expires,
    From,
    Host,
    Http2Settings,
    IfMatch,
    IfModifiedSince,
    IfNoneMatch,
    IfRange,
    IfUnmodifiedSince,
    KeepAlive,
    LastModified,
    Location,
    MaxForwards,
    Pragma,
    ProxyAuthenticate,
    ProxyAuthorization,
    Range,
    Referer,
    RetryAfter,
    Server,
    SetCookie,
    Te,
    Trailer,
    TransferEncoding,
    Upgrade,
    UserAgent,
    Vary,
    Via,
    Warning,
    WwwAuthenticate,
    XForwardedFor,
}

impl KnownHeader {
    pub fn canonical_name(&self) -> &'static str {
        match self {
            Self::Accept => "Accept",
            Self::AcceptCharset => "Accept-Charset",
            Self::AcceptEncoding => "Accept-Encoding",
            Self::AcceptLanguage => "Accept-Language",
            Self::AcceptRanges => "Accept-Ranges",
            Self::Age => "Age",
            Self::Allow => "Allow",
            Self::Authorization => "Authorization",
            Self::CacheControl => "Cache-Control",
            Self::Connection => "Connection",
            Self::ContentEncoding => "Content-Encoding",
            Self::ContentLanguage => "Content-Language",
            Self::ContentLength => "Content-Length",
            Self::ContentLocation => "Content-Location",
            Self::ContentMd5 => "Content-MD5",
            Self::ContentRange => "Content-Range",
            Self::ContentType => "Content-Type",
            Self::Cookie => "Cookie",
            Self::Date => "Date",
            Self::ETag => "ETag",
            Self::Expect => "Expect",
            Self::Expires => "Expires",
            Self::From => "From",
            Self::Host => "Host",
            Self::Http2Settings => "HTTP2-Settings",
            Self::IfMatch => "If-Match",
            Self::IfModifiedSince => "If-Modified-Since",
            Self::IfNoneMatch => "If-None-Match",
            Self::IfRange => "If-Range",
            Self::IfUnmodifiedSince => "If-Unmodified-Since",
            Self::KeepAlive => "Keep-Alive",
            Self::LastModified => "Last-Modified",
            Self::Location => "Location",
            Self::MaxForwards => "Max-Forwards",
            Self::Pragma => "Pragma",
            Self::ProxyAuthenticate => "Proxy-Authenticate",
            Self::ProxyAuthorization => "Proxy-Authorization",
            Self::Range => "Range",
            Self::Referer => "Referer",
            Self::RetryAfter => "Retry-After",
            Self::Server => "Server",
            Self::SetCookie => "Set-Cookie",
            Self::Te => "TE",
            Self::Trailer => "Trailer",
            Self::TransferEncoding => "Transfer-Encoding",
            Self::Upgrade => "Upgrade",
            Self::UserAgent => "User-Agent",
            Self::Vary => "Vary",
            Self::Via => "Via",
            Self::Warning => "Warning",
            Self::WwwAuthenticate => "WWW-Authenticate",
            Self::XForwardedFor => "X-Forwarded-For",
        }
    }

    pub fn flags(&self) -> HeaderFlags {
        let p = HeaderFlags::PASS;
        let f = HeaderFlags::FETCH;
        let i = HeaderFlags::INSERT;
        let s = HeaderFlags::PASS_S;
        let k = HeaderFlags::CONNECTION;

        match self {
            Self::AcceptRanges => p | f | i,
            Self::Age => i | s,
            Self::CacheControl => f,
            Self::Connection => p | f | i | s | k,
            Self::ContentRange => f | i,
            Self::Http2Settings => p | f | i | s | k,
            Self::IfMatch => f,
            Self::IfModifiedSince => f,
            Self::IfNoneMatch => f,
            Self::IfRange => f,
            Self::IfUnmodifiedSince => f,
            Self::KeepAlive => p | f | i | s | k,
            Self::ProxyAuthenticate => f | i,
            Self::ProxyAuthorization => f | i,
            Self::Range => f | i,
            Self::Te => p | f | i | s,
            Self::Trailer => p | f | i | s,
            Self::TransferEncoding => p | f | i | s | k,
            Self::Upgrade => p | f | i | s | k,
            _ => HeaderFlags::empty(),
        }
    }

    /// Try to match a header name (case-insensitive) to a known header.
    pub fn from_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "accept" => Some(Self::Accept),
            "accept-charset" => Some(Self::AcceptCharset),
            "accept-encoding" => Some(Self::AcceptEncoding),
            "accept-language" => Some(Self::AcceptLanguage),
            "accept-ranges" => Some(Self::AcceptRanges),
            "age" => Some(Self::Age),
            "allow" => Some(Self::Allow),
            "authorization" => Some(Self::Authorization),
            "cache-control" => Some(Self::CacheControl),
            "connection" => Some(Self::Connection),
            "content-encoding" => Some(Self::ContentEncoding),
            "content-language" => Some(Self::ContentLanguage),
            "content-length" => Some(Self::ContentLength),
            "content-location" => Some(Self::ContentLocation),
            "content-md5" => Some(Self::ContentMd5),
            "content-range" => Some(Self::ContentRange),
            "content-type" => Some(Self::ContentType),
            "cookie" => Some(Self::Cookie),
            "date" => Some(Self::Date),
            "etag" => Some(Self::ETag),
            "expect" => Some(Self::Expect),
            "expires" => Some(Self::Expires),
            "from" => Some(Self::From),
            "host" => Some(Self::Host),
            "http2-settings" => Some(Self::Http2Settings),
            "if-match" => Some(Self::IfMatch),
            "if-modified-since" => Some(Self::IfModifiedSince),
            "if-none-match" => Some(Self::IfNoneMatch),
            "if-range" => Some(Self::IfRange),
            "if-unmodified-since" => Some(Self::IfUnmodifiedSince),
            "keep-alive" => Some(Self::KeepAlive),
            "last-modified" => Some(Self::LastModified),
            "location" => Some(Self::Location),
            "max-forwards" => Some(Self::MaxForwards),
            "pragma" => Some(Self::Pragma),
            "proxy-authenticate" => Some(Self::ProxyAuthenticate),
            "proxy-authorization" => Some(Self::ProxyAuthorization),
            "range" => Some(Self::Range),
            "referer" => Some(Self::Referer),
            "retry-after" => Some(Self::RetryAfter),
            "server" => Some(Self::Server),
            "set-cookie" => Some(Self::SetCookie),
            "te" => Some(Self::Te),
            "trailer" => Some(Self::Trailer),
            "transfer-encoding" => Some(Self::TransferEncoding),
            "upgrade" => Some(Self::Upgrade),
            "user-agent" => Some(Self::UserAgent),
            "vary" => Some(Self::Vary),
            "via" => Some(Self::Via),
            "warning" => Some(Self::Warning),
            "www-authenticate" => Some(Self::WwwAuthenticate),
            "x-forwarded-for" => Some(Self::XForwardedFor),
            _ => None,
        }
    }
}

impl std::fmt::Display for KnownHeader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.canonical_name())
    }
}
