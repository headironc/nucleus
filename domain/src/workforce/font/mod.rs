//! Bounded standalone SFNT input, synchronous sanitization and exact byte identities.

use std::{
    collections::TryReserveError,
    io::{self, Seek, SeekFrom, Write},
};

use sha2::{Digest, Sha256};
use thiserror::Error;
/// Maximum accepted original upload and sanitized output size in bytes.
pub const MAX_SIZE: u64 = 32 * 1024 * 1024;

const TRUETYPE_MAGIC: [u8; 4] = [0x00, 0x01, 0x00, 0x00];

/// The supported media types of a sanitized standalone OpenType font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    /// TrueType outlines in an SFNT container.
    TrueType,
    /// OpenType outlines in an SFNT container.
    OpenType,
}

impl MediaType {
    /// The media type stored with the sanitized Font bytes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TrueType => "font/ttf",
            Self::OpenType => "font/otf",
        }
    }

    fn of(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(&TRUETYPE_MAGIC) {
            Some(Self::TrueType)
        } else if bytes.starts_with(b"OTTO") {
            Some(Self::OpenType)
        } else {
            None
        }
    }
}

/// The SHA-256 digest and exact byte length of one original upload or sanitized output.
///
/// Only an accepted [`Upload`] or a successful sanitization constructs this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    digest: [u8; 32],
    size: u64,
}

impl Fingerprint {
    /// The SHA-256 digest of the identified bytes.
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    /// The exact byte length of the identified bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    fn of(bytes: &[u8], size: u64) -> Self {
        Self {
            digest: Sha256::digest(bytes).into(),
            size,
        }
    }
}

/// One original OpenType SFNT upload bounded by [`MAX_SIZE`].
///
/// Construction accepts only standalone TrueType and OpenType SFNT files. Collections and
/// WOFF/WOFF2 wrappers are rejected before any decoder runs. Accepting an upload does not
/// establish complete font validity.
pub struct Upload {
    bytes: Vec<u8>,
    fingerprint: Fingerprint,
}

impl Upload {
    /// Admits bounded original upload bytes without invoking a font decoder.
    ///
    /// # Errors
    ///
    /// Returns [`InputError::Empty`] or [`InputError::TooLarge`] when the byte budget is violated,
    /// and [`InputError::UnsupportedFormat`] for collections, compressed wrappers and every other
    /// magic.
    pub fn new(bytes: Vec<u8>) -> Result<Self, InputError> {
        if bytes.is_empty() {
            return Err(InputError::Empty);
        }
        let size = match u64::try_from(bytes.len()) {
            Ok(size) => size,
            Err(_) => return Err(InputError::TooLarge),
        };
        if size > MAX_SIZE {
            return Err(InputError::TooLarge);
        }
        if MediaType::of(&bytes).is_none() {
            return Err(InputError::UnsupportedFormat);
        }
        Ok(Self {
            fingerprint: Fingerprint::of(&bytes, size),
            bytes,
        })
    }

    /// The original upload identity used to match an original request.
    ///
    /// This identity covers the uploaded bytes, not the sanitized output.
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// Sanitizes this upload synchronously within the fixed output bound.
    ///
    /// The returned output is nonempty and no larger than [`MAX_SIZE`]. Success establishes only
    /// that fontsan accepted and rewrote these bytes; it does not establish glyph coverage,
    /// optional-table semantics, licensing or formal print output. Partial output from a failed
    /// sanitization is discarded.
    ///
    /// This call performs native CPU and memory work in the calling thread. It neither acquires
    /// nor releases a work permit and must run inside the transaction owner's retained blocking
    /// worker. The output bound does not bound every native allocation or the sanitizer's CPU
    /// time.
    pub fn sanitize(&self) -> Result<Prepared, InputError> {
        self.sanitize_with_limit(MAX_SIZE)
    }

    fn sanitize_with_limit(&self, limit: u64) -> Result<Prepared, InputError> {
        let limit = limit.min(MAX_SIZE);
        let mut output = Output::new(limit);
        let result = fontsan::process_and_write(&mut output, &self.bytes);
        // A definite failure of this bounded output is independent of the library's outcome.
        if let Some(fault) = output.fault.take() {
            return Err(fault.into_error());
        }
        match result {
            Ok(()) => {}
            Err(fontsan::Error::InvalidFont) => return Err(InputError::InvalidFont),
            Err(fontsan::Error::IoError(error)) => return Err(InputError::OutputWrite(error)),
        }
        let bytes = output.into_bytes();
        if bytes.is_empty() {
            return Err(InputError::OutputEmpty);
        }
        let size = match u64::try_from(bytes.len()) {
            Ok(size) => size,
            Err(_) => return Err(InputError::OutputLimit),
        };
        if size > limit {
            return Err(InputError::OutputLimit);
        }
        let media_type = MediaType::of(&bytes).ok_or(InputError::OutputFormat)?;
        Ok(Prepared {
            fingerprint: Fingerprint::of(&bytes, size),
            bytes,
            media_type,
        })
    }
}

/// Sanitized Font plaintext together with the identity and media type of those exact bytes.
pub struct Prepared {
    bytes: Vec<u8>,
    fingerprint: Fingerprint,
    media_type: MediaType,
}

impl Prepared {
    /// The sanitized plaintext that the file facility stores for this Font.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The SHA-256 and exact length of [`Prepared::as_bytes`].
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// The supported SFNT media type of [`Prepared::as_bytes`].
    pub fn media_type(&self) -> MediaType {
        self.media_type
    }
}

/// Why an upload was not admitted or sanitized into a prepared Font.
#[derive(Debug, Error)]
pub enum InputError {
    #[error("font upload is empty")]
    Empty,
    #[error("font upload exceeds the size limit")]
    TooLarge,
    #[error("font upload is not a supported standalone SFNT font")]
    UnsupportedFormat,
    #[error("font sanitization rejected the input")]
    InvalidFont,
    #[error("sanitized font output exceeds its size limit")]
    OutputLimit,
    #[error("cannot allocate sanitized font output")]
    OutputAllocation(#[source] TryReserveError),
    #[error("cannot write sanitized font output: {0}")]
    OutputWrite(#[source] io::Error),
    #[error("sanitized font output was positioned before its start")]
    OutputSeek,
    #[error("font sanitization produced no output")]
    OutputEmpty,
    #[error("font sanitization produced an unsupported SFNT container")]
    OutputFormat,
}

/// The bounded in-memory output stream passed to fontsan.
struct Output {
    bytes: Vec<u8>,
    position: u64,
    limit: u64,
    fault: Option<OutputFault>,
}

/// The first definite failure of the bounded output.
enum OutputFault {
    Limit,
    Allocation(TryReserveError),
    Seek,
}

impl Output {
    fn new(limit: u64) -> Self {
        Self {
            bytes: Vec::new(),
            position: 0,
            limit,
            fault: None,
        }
    }

    fn failure(&mut self, fault: OutputFault) -> io::Error {
        let error = fault.io_error();
        self.fault = Some(fault);
        error
    }

    fn recorded(&self) -> io::Error {
        self.fault.as_ref().map_or_else(
            || io::Error::other("sanitized font output failed"),
            OutputFault::io_error,
        )
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl OutputFault {
    fn io_error(&self) -> io::Error {
        match self {
            Self::Limit => io::Error::other("sanitized font output exceeds its size limit"),
            Self::Allocation(_) => io::Error::other("cannot allocate sanitized font output"),
            Self::Seek => io::Error::new(
                io::ErrorKind::InvalidInput,
                "sanitized font output would move before its start",
            ),
        }
    }

    fn into_error(self) -> InputError {
        match self {
            Self::Limit => InputError::OutputLimit,
            Self::Allocation(error) => InputError::OutputAllocation(error),
            Self::Seek => InputError::OutputSeek,
        }
    }
}

impl Write for Output {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.fault.is_some() {
            return Err(self.recorded());
        }
        if data.is_empty() {
            return Ok(0);
        }
        let length = match u64::try_from(data.len()) {
            Ok(length) => length,
            Err(_) => return Err(self.failure(OutputFault::Limit)),
        };
        let end = match self.position.checked_add(length) {
            Some(end) if end <= self.limit => end,
            _ => return Err(self.failure(OutputFault::Limit)),
        };
        let end_index = match usize::try_from(end) {
            Ok(end) => end,
            Err(_) => return Err(self.failure(OutputFault::Limit)),
        };
        if end_index > self.bytes.len() {
            if let Err(error) = self.bytes.try_reserve_exact(end_index - self.bytes.len()) {
                return Err(self.failure(OutputFault::Allocation(error)));
            }
            self.bytes.resize(end_index, 0);
        }
        let start = match usize::try_from(self.position) {
            Ok(start) => start,
            Err(_) => return Err(self.failure(OutputFault::Limit)),
        };
        self.bytes[start..end_index].copy_from_slice(data);
        self.position = end;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for Output {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        // fontsan's Tell callback uses `Current(0)` and panics if it fails.
        if from == SeekFrom::Current(0) {
            return Ok(self.position);
        }
        if self.fault.is_some() {
            return Err(self.recorded());
        }
        let target = match from {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::Current(value) => i128::from(self.position) + i128::from(value),
            SeekFrom::End(value) => {
                let length = match u64::try_from(self.bytes.len()) {
                    Ok(length) => length,
                    Err(_) => return Err(self.failure(OutputFault::Limit)),
                };
                i128::from(length) + i128::from(value)
            }
        };
        if target < 0 {
            return Err(self.failure(OutputFault::Seek));
        }
        let target = match u64::try_from(target) {
            Ok(target) => target,
            Err(_) => return Err(self.failure(OutputFault::Limit)),
        };
        if target > self.limit {
            return Err(self.failure(OutputFault::Limit));
        }
        self.position = target;
        Ok(target)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::io::{Seek, SeekFrom, Write};

    use sha2::{Digest, Sha256};

    use crate::workforce::font::{InputError, MAX_SIZE, MediaType, Output, OutputFault, Upload};

    const TTF: &[u8] = include_bytes!("../../../tests/fixtures/fonts/SourceSans3-Regular.ttf");
    const OTF: &[u8] = include_bytes!("../../../tests/fixtures/fonts/SourceSans3-Regular.otf");
    const TTF_SHA256: &str = "4644c81b86ec9caaa76b634889968ed3c4f4f52f054855933acc7c2b21e53b0f";
    const OTF_SHA256: &str = "08df266400933d3178d081a45f94a08814c3e55b4b7dd2e0ff69cb1329f13ab6";

    fn size(bytes: &[u8]) -> u64 {
        u64::try_from(bytes.len()).expect("test byte length fits u64")
    }

    #[test]
    fn pinned_fixtures_keep_source_and_output_identities() {
        for (bytes, expected, media_type) in [
            (TTF, TTF_SHA256, MediaType::TrueType),
            (OTF, OTF_SHA256, MediaType::OpenType),
        ] {
            let source: [u8; 32] = Sha256::digest(bytes).into();
            assert_eq!(hex::encode(source), expected);
            let upload = Upload::new(bytes.to_vec()).expect("supported fixture upload");
            assert_eq!(upload.fingerprint().digest(), &source);
            assert_eq!(upload.fingerprint().size(), size(bytes));

            let prepared = upload.sanitize().expect("sanitize fixture");
            assert!(!prepared.as_bytes().is_empty());
            assert!(size(prepared.as_bytes()) <= MAX_SIZE);
            assert_eq!(prepared.media_type(), media_type);
            let output: [u8; 32] = Sha256::digest(prepared.as_bytes()).into();
            assert_eq!(prepared.fingerprint().digest(), &output);
            assert_eq!(prepared.fingerprint().size(), size(prepared.as_bytes()));

            // Sanitization neither rewrites the source identity nor makes it the output identity.
            assert_eq!(upload.fingerprint().digest(), &source);
            let repeated = upload.sanitize().expect("repeat sanitization");
            assert_eq!(repeated.as_bytes(), prepared.as_bytes());
        }
    }

    #[test]
    fn empty_and_oversized_uploads_are_rejected_before_decoding() {
        assert!(matches!(Upload::new(Vec::new()), Err(InputError::Empty)));

        let bound = usize::try_from(MAX_SIZE).expect("bound fits usize");
        let oversized = vec![0; bound + 1];
        assert!(matches!(Upload::new(oversized), Err(InputError::TooLarge)));

        // The exact bound passes the budget check and then fails the format gate.
        let exact = vec![0; bound];
        assert!(matches!(
            Upload::new(exact),
            Err(InputError::UnsupportedFormat)
        ));
    }

    #[test]
    fn unsupported_containers_are_rejected_before_sanitization() {
        for magic in [b"ttcf", b"wOFF", b"wOF2", b"true", b"typ1"] {
            let mut bytes = vec![0; 12];
            bytes[..4].copy_from_slice(magic);
            assert!(matches!(
                Upload::new(bytes),
                Err(InputError::UnsupportedFormat)
            ));
        }
    }

    #[test]
    fn truncated_and_damaged_fonts_are_invalid_not_unsupported() {
        // The fixed head offsets are only valid for the digest-pinned fixtures.
        let ttf: [u8; 32] = Sha256::digest(TTF).into();
        let otf: [u8; 32] = Sha256::digest(OTF).into();
        assert_eq!(hex::encode(ttf), TTF_SHA256);
        assert_eq!(hex::encode(otf), OTF_SHA256);

        for (bytes, head) in [(TTF, 373_680usize), (OTF, 236usize)] {
            let mut damaged = bytes.to_vec();
            damaged[head + 12..head + 16].copy_from_slice(&0u32.to_be_bytes());
            assert!(matches!(
                Upload::new(damaged)
                    .expect("magic passes the format gate")
                    .sanitize(),
                Err(InputError::InvalidFont)
            ));

            let truncated = bytes[..bytes.len() / 2].to_vec();
            assert!(matches!(
                Upload::new(truncated)
                    .expect("magic passes the format gate")
                    .sanitize(),
                Err(InputError::InvalidFont)
            ));
        }

        for bytes in [&TTF[..11], &OTF[..11]] {
            assert!(matches!(
                Upload::new(bytes.to_vec())
                    .expect("magic passes the format gate")
                    .sanitize(),
                Err(InputError::InvalidFont)
            ));
        }

        let mut directory = TTF.to_vec();
        directory[4..6].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(matches!(
            Upload::new(directory)
                .expect("magic passes the format gate")
                .sanitize(),
            Err(InputError::InvalidFont)
        ));
    }

    #[test]
    fn output_limit_is_classified_before_invalid_font() {
        let upload = Upload::new(TTF.to_vec()).expect("supported fixture upload");
        // fontsan reports InvalidFont for its write failure; the definite output fault wins.
        assert!(matches!(
            upload.sanitize_with_limit(64),
            Err(InputError::OutputLimit)
        ));

        // The bounded stream may hold partial bytes, but only the error leaves sanitization.
        let mut output = Output::new(64);
        assert!(fontsan::process_and_write(&mut output, TTF).is_err());
        assert!(matches!(&output.fault, Some(OutputFault::Limit)));
        assert!(output.bytes.len() <= 64);
    }

    #[test]
    fn exact_output_bound_is_inclusive() {
        let upload = Upload::new(TTF.to_vec()).expect("supported fixture upload");
        let prepared = upload.sanitize().expect("sanitize fixture");
        let length = size(prepared.as_bytes());
        let exact = upload
            .sanitize_with_limit(length)
            .expect("output fits its exact size");
        assert_eq!(exact.as_bytes(), prepared.as_bytes());
        assert!(matches!(
            upload.sanitize_with_limit(length - 1),
            Err(InputError::OutputLimit)
        ));
    }

    #[test]
    fn exact_bound_write_is_accepted_and_the_next_byte_is_not() {
        let mut output = Output::new(2);
        output.write_all(&[7, 8]).expect("write the exact bound");
        assert_eq!(output.bytes.as_slice(), &[7, 8]);
        assert!(output.write_all(&[9]).is_err());
        assert!(matches!(&output.fault, Some(OutputFault::Limit)));
        assert_eq!(output.bytes.as_slice(), &[7, 8]);
        assert_eq!(output.stream_position().expect("tell"), 2);
    }

    #[test]
    fn output_seeks_are_bounded_and_tell_remains_infallible() {
        let mut output = Output::new(MAX_SIZE);
        assert_eq!(output.stream_position().expect("tell"), 0);
        assert_eq!(
            output
                .seek(SeekFrom::Start(MAX_SIZE))
                .expect("seek to the exact bound"),
            MAX_SIZE
        );
        assert!(output.bytes.is_empty());
        assert!(output.write_all(&[1]).is_err());
        assert!(matches!(&output.fault, Some(OutputFault::Limit)));
        assert!(output.bytes.is_empty());
        assert_eq!(
            output.stream_position().expect("tell after failure"),
            MAX_SIZE
        );

        let mut negative = Output::new(MAX_SIZE);
        assert!(negative.seek(SeekFrom::Current(-1)).is_err());
        assert!(matches!(&negative.fault, Some(OutputFault::Seek)));
        assert!(negative.bytes.is_empty());
        assert_eq!(negative.stream_position().expect("tell after failure"), 0);

        let mut before_start = Output::new(MAX_SIZE);
        assert!(before_start.seek(SeekFrom::End(-1)).is_err());
        assert!(matches!(&before_start.fault, Some(OutputFault::Seek)));
        assert_eq!(
            before_start.stream_position().expect("tell after failure"),
            0
        );

        let mut huge = Output::new(MAX_SIZE);
        assert!(huge.seek(SeekFrom::Start(u64::MAX)).is_err());
        assert!(matches!(&huge.fault, Some(OutputFault::Limit)));
        assert!(huge.bytes.is_empty());
        assert_eq!(huge.stream_position().expect("tell after failure"), 0);

        let mut sparse = Output::new(MAX_SIZE);
        sparse.seek(SeekFrom::Start(4)).expect("seek past the end");
        assert_eq!(sparse.write(&[]).expect("empty write is a no-op"), 0);
        assert!(sparse.bytes.is_empty());
        sparse.write_all(&[1]).expect("write after a sparse seek");
        assert_eq!(sparse.bytes.as_slice(), &[0, 0, 0, 0, 1]);
    }
}
