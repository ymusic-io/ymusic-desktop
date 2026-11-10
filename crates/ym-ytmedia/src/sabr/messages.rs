//! SABR protobuf messages. Field numbers are the wire truth (YMusicV4 `yt-media/SABR.md` §3).

use crate::proto::{Message, ProtoError, ProtoWriter};

/// Groups a format stream's bytes; the same string as the Kotlin cache key.
pub fn format_cache_key(itag: i32, lmt: i64) -> String {
    format!("2_{itag}_{lmt}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatId {
    pub itag: i32,
    pub lmt: i64,
    pub xtag: String,
}

impl FormatId {
    pub fn encode(&self) -> ProtoWriter {
        ProtoWriter::new().varint(1, Some(i64::from(self.itag))).varint(2, Some(self.lmt)).string(3, Some(&self.xtag))
    }
}

/// One `streamingData.adaptiveFormats` entry: what the protocol and format picking read.
#[derive(Clone, Debug, PartialEq)]
pub struct AdaptiveFormat {
    pub itag: i32,
    pub lmt: i64,
    pub xtag: Option<String>,
    pub mime_type: String,
    /// 0 for live streams, which carry no length.
    pub clen: i64,
    pub bitrate: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub fps: Option<i32>,
    pub quality_label: Option<String>,
    pub approx_duration_ms: i64,
    pub audio_track_is_default: Option<bool>,
    pub audio_track_id: Option<String>,
    pub audio_track_name: Option<String>,
    pub is_drc: bool,
    /// This stream's own measured loudness: DRC and other variants of one video differ.
    pub loudness_lkfs: Option<f64>,
    pub is_vb: bool,
}

impl AdaptiveFormat {
    pub fn from_json(json: &serde_json::Value) -> Option<Self> {
        let long = |key: &str| {
            json.get(key).and_then(|value| value.as_i64().or_else(|| value.as_str().and_then(|text| text.parse().ok())))
        };
        let string = |key: &str| json.get(key).and_then(serde_json::Value::as_str).map(str::to_owned);
        let audio_track = json.get("audioTrack");
        Some(Self {
            itag: i32::try_from(long("itag")?).ok()?,
            lmt: long("lastModified").unwrap_or(0),
            xtag: string("xtags"),
            mime_type: string("mimeType").unwrap_or_default(),
            clen: long("contentLength").unwrap_or(0),
            bitrate: long("bitrate").unwrap_or(0),
            width: long("width").and_then(|value| i32::try_from(value).ok()).filter(|value| *value > 0),
            height: long("height").and_then(|value| i32::try_from(value).ok()).filter(|value| *value > 0),
            fps: long("fps").and_then(|value| i32::try_from(value).ok()).filter(|value| *value > 0),
            quality_label: string("qualityLabel"),
            approx_duration_ms: long("approxDurationMs").unwrap_or(0),
            audio_track_is_default: audio_track
                .and_then(|track| track.get("audioIsDefault"))
                .and_then(serde_json::Value::as_bool),
            audio_track_id: audio_track.and_then(|track| track.get("id")).and_then(|id| id.as_str()).map(str::to_owned),
            audio_track_name: audio_track
                .and_then(|track| track.get("displayName"))
                .and_then(|name| name.as_str())
                .map(str::to_owned),
            is_drc: json.get("isDrc").and_then(serde_json::Value::as_bool).unwrap_or(false),
            loudness_lkfs: json.get("trackAbsoluteLoudnessLkfs").and_then(serde_json::Value::as_f64),
            is_vb: json.get("isVb").and_then(serde_json::Value::as_bool).unwrap_or(false),
        })
    }

    pub fn is_video(&self) -> bool {
        self.mime_type.starts_with("video")
    }

    pub fn mime_without_codec(&self) -> &str {
        self.mime_type.split(';').next().unwrap_or_default().trim()
    }

    pub fn cache_key(&self) -> String {
        format_cache_key(self.itag, self.lmt)
    }

    pub fn format_id(&self) -> FormatId {
        FormatId { itag: self.itag, lmt: self.lmt, xtag: self.xtag.clone().unwrap_or_default() }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClientAbrState {
    pub player_time_ms: i64,
    /// 0 = audio and video, 1 = audio only (the server sends no video segments).
    pub enabled_track_types_bitfield: Option<i64>,
    pub drc_enabled: bool,
    pub request_audio_track_id: Option<String>,
    /// min(width, height); none lets the server pick.
    pub requested_video_resolution: Option<i64>,
    pub viewport_width: Option<i64>,
    pub viewport_height: Option<i64>,
    /// Must be > 0.
    pub time_since_last_seek: Option<i64>,
    pub visibility: Option<i64>,
    pub elapsed_wall_time_ms: Option<i64>,
    pub time_since_last_action: Option<i64>,
}

impl ClientAbrState {
    pub fn encode(&self) -> ProtoWriter {
        ProtoWriter::new()
            .varint(28, Some(self.player_time_ms))
            .varint(40, self.enabled_track_types_bitfield)
            .varint(46, self.drc_enabled.then_some(1))
            .string(69, self.request_audio_track_id.as_deref())
            .varint(16, self.requested_video_resolution)
            .varint(18, self.viewport_width)
            .varint(19, self.viewport_height)
            .varint(29, self.time_since_last_seek)
            .varint(34, self.visibility)
            .varint(36, self.elapsed_wall_time_ms)
            .varint(39, self.time_since_last_action)
    }
}

/// What the client holds of one format, as whole segments.
#[derive(Clone, Debug)]
pub struct BufferedRange {
    pub format_id: FormatId,
    pub start_ms: i64,
    pub duration_ms: i64,
    pub start_segment_index: i64,
    pub end_segment_index: i64,
}

impl BufferedRange {
    /// Exclusive: the range covers `start_ms <= t < end_ms`.
    pub fn end_ms(&self) -> i64 {
        self.start_ms + self.duration_ms
    }

    pub fn encode(&self) -> ProtoWriter {
        ProtoWriter::new()
            .message(1, Some(self.format_id.encode()))
            .varint(2, Some(self.start_ms))
            .varint(3, Some(self.duration_ms))
            .varint(4, Some(self.start_segment_index))
            .varint(5, Some(self.end_segment_index))
    }
}

/// Must describe the InnerTube client that produced the player response.
#[derive(Clone, Debug, Default)]
pub struct ClientInfo {
    pub client_name: Option<i64>,
    pub client_version: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
}

impl ClientInfo {
    pub fn encode(&self) -> ProtoWriter {
        ProtoWriter::new()
            .varint(16, self.client_name)
            .string(17, self.client_version.as_deref())
            .string(18, self.os_name.as_deref())
            .string(19, self.os_version.as_deref())
    }
}

#[derive(Clone, Debug)]
pub struct SabrContextUpdate {
    pub context_type: i64,
    pub value: Vec<u8>,
}

impl SabrContextUpdate {
    pub fn from_ump(message: &Message) -> Result<Self, ProtoError> {
        Ok(Self {
            context_type: message.require_varint(1)? as i64,
            value: message.bytes(3).ok_or(ProtoError::Missing { field: 3 })?.to_vec(),
        })
    }

    /// The response's `value` (3) goes back as field 2.
    pub fn encode(&self) -> ProtoWriter {
        ProtoWriter::new().varint(1, Some(self.context_type)).bytes(2, Some(&self.value))
    }
}

pub struct VideoPlaybackAbrRequest<'a> {
    pub client_abr_state: ClientAbrState,
    /// Formats whose init segment the client holds.
    pub selected_formats: Vec<FormatId>,
    pub buffered_ranges: Vec<BufferedRange>,
    pub ustreamer_config: &'a [u8],
    pub selected_audio_formats: Vec<FormatId>,
    pub selected_video_formats: Vec<FormatId>,
    pub client_info: &'a ClientInfo,
    pub po_token: Option<&'a [u8]>,
    pub playback_cookie: &'a [u8],
    pub sabr_contexts: Vec<&'a SabrContextUpdate>,
}

impl VideoPlaybackAbrRequest<'_> {
    pub fn encode(&self) -> Vec<u8> {
        let streamer_context = ProtoWriter::new()
            .message(1, Some(self.client_info.encode()))
            .bytes(2, self.po_token)
            .bytes(3, Some(self.playback_cookie))
            .messages(5, self.sabr_contexts.iter().map(|update| update.encode()));
        ProtoWriter::new()
            .message(1, Some(self.client_abr_state.encode()))
            .messages(2, self.selected_formats.iter().map(FormatId::encode))
            .messages(3, self.buffered_ranges.iter().map(BufferedRange::encode))
            .bytes(5, Some(self.ustreamer_config))
            .messages(16, self.selected_audio_formats.iter().map(FormatId::encode))
            .messages(17, self.selected_video_formats.iter().map(FormatId::encode))
            .message(19, Some(streamer_context))
            .into_bytes()
    }
}

#[derive(Clone, Debug)]
pub struct MediaHeader {
    pub header_id: u8,
    pub video_id: String,
    pub itag: i32,
    pub lmt: i64,
    pub xtag: Option<String>,
    /// Byte offset of this segment within the format's contiguous stream.
    pub start_range: i64,
    pub is_init_segment: bool,
    /// 0 for the init segment.
    pub segment_index: i64,
    pub start_ms: Option<i64>,
    pub duration_ms: Option<i64>,
    /// The whole segment's length, even when this response resumes it part-way.
    pub content_length: i64,
    /// Where this response's MEDIA_DATA starts inside the segment; non-zero when the server resumes
    /// a segment an earlier response delivered in part.
    pub data_offset: i64,
}

impl MediaHeader {
    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let message = Message::parse(payload)?;
        Ok(Self {
            header_id: message.require_varint(1)? as u8,
            video_id: message.string(2)?.unwrap_or_default(),
            itag: message.require_varint(3)? as i32,
            lmt: message.varint(4).unwrap_or(0) as i64,
            xtag: message.string(5)?,
            start_range: message.varint(6).unwrap_or(0) as i64,
            is_init_segment: message.varint(8) == Some(1),
            segment_index: message.varint(9).unwrap_or(0) as i64,
            start_ms: message.varint(11).map(|value| value as i64),
            duration_ms: message.varint(12).map(|value| value as i64),
            content_length: message.varint(14).unwrap_or(0) as i64,
            data_offset: message.varint(17).unwrap_or(0) as i64,
        })
    }

    pub fn cache_key(&self) -> String {
        format_cache_key(self.itag, self.lmt)
    }

    pub fn describe(&self) -> String {
        format!(
            "itag={} seg={} range={}+{} dataOffset={}",
            self.itag, self.segment_index, self.start_range, self.content_length, self.data_offset
        )
    }
}

pub struct NextRequestPolicy {
    pub backoff_time_ms: Option<u64>,
    pub playback_cookie: Option<Vec<u8>>,
}

impl NextRequestPolicy {
    pub fn parse(payload: &[u8]) -> Result<Self, ProtoError> {
        let message = Message::parse(payload)?;
        Ok(Self { backoff_time_ms: message.varint(4), playback_cookie: message.bytes(7).map(<[u8]>::to_vec) })
    }
}

/// Part 44's message, or part 43's replacement URL: both are string field 1.
pub fn string_field_1(payload: &[u8]) -> Option<String> {
    Message::parse(payload).ok()?.string(1).ok()?
}

/// Part 58: 1 = ok, 2 = attestation pending, 3 = invalid PO token (media stops).
pub fn stream_protection_status(payload: &[u8]) -> Option<u64> {
    Message::parse(payload).ok()?.varint(1)
}
