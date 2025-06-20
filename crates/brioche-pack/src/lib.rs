use std::io::SeekFrom;

use encoding::TickEncoded;

mod encoding;

const MARKER: &[u8; 32] = b"brioche_pack_v0                 ";

const LENGTH_BYTES: usize = 4;
type LengthInt = u32;

#[serde_with::serde_as]
#[derive(Debug, bincode::Encode, bincode::Decode, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum Pack {
    #[serde(rename_all = "camelCase")]
    LdLinux {
        #[serde_as(as = "TickEncoded")]
        program: Vec<u8>,
        #[serde_as(as = "TickEncoded")]
        interpreter: Vec<u8>,
        #[serde_as(as = "Vec<TickEncoded>")]
        library_dirs: Vec<Vec<u8>>,
        #[serde_as(as = "Vec<TickEncoded>")]
        runtime_library_dirs: Vec<Vec<u8>>,
    },
    #[serde(rename_all = "camelCase")]
    Static {
        #[serde_as(as = "Vec<TickEncoded>")]
        library_dirs: Vec<Vec<u8>>,
    },
    #[serde(rename_all = "camelCase")]
    Metadata {
        #[serde_as(as = "Vec<TickEncoded>")]
        resource_paths: Vec<Vec<u8>>,
        format: String,
        #[serde_as(as = "TickEncoded")]
        metadata: Vec<u8>,
    },
}

impl Pack {
    pub fn paths(&self) -> Vec<bstr::BString> {
        let mut paths = vec![];

        match self {
            Self::LdLinux {
                program,
                interpreter,
                library_dirs,
                runtime_library_dirs: _,
            } => {
                paths.push(bstr::BString::from(program.clone()));
                paths.push(bstr::BString::from(interpreter.clone()));
                paths.extend(library_dirs.iter().cloned().map(bstr::BString::from));
            }
            Self::Static { library_dirs } => {
                paths.extend(library_dirs.iter().cloned().map(bstr::BString::from));
            }
            Self::Metadata { resource_paths, .. } => {
                paths.extend(resource_paths.iter().cloned().map(bstr::BString::from));
            }
        }

        paths
    }

    #[must_use]
    pub const fn should_add_to_executable(&self) -> bool {
        match self {
            Self::LdLinux { .. } => true,
            Self::Static { library_dirs } => {
                // If the executable is statically linked but contains no
                // dynamically-linked libraries, then we have no reason to
                // add it to the executable
                !library_dirs.is_empty()
            }
            Self::Metadata { .. } => true,
        }
    }
}

pub fn inject_pack(mut writer: impl std::io::Write, pack: &Pack) -> Result<usize, InjectPackError> {
    // Encode the pack
    let pack_bytes = bincode::encode_to_vec(pack, bincode::config::standard())
        .map_err(InjectPackError::SerializeError)?;

    // Get the encoded length as a little-endian array of bytes
    let pack_length: LengthInt = pack_bytes
        .len()
        .try_into()
        .map_err(|_| InjectPackError::PackTooLarge)?;
    let length_bytes = pack_length.to_le_bytes();

    // Write the marker, length, pack, length, and marker. Writing the marker
    // at each end makes it possible to detect the marker both by reading from
    // a stream where we know the unpacked size, and by seeking to the end
    // and reading backwards when we don't know the unpacked size
    writer.write_all(MARKER)?;
    writer.write_all(&length_bytes)?;
    writer.write_all(&pack_bytes)?;
    writer.write_all(&length_bytes)?;
    writer.write_all(MARKER)?;

    // Return the total length of the marker, length, and pack data appended
    let pack_length = (MARKER.len() + LENGTH_BYTES) * 2 + pack_bytes.len();
    Ok(pack_length)
}

pub struct ExtractedPack {
    pub pack: Pack,
    pub unpacked_len: usize,
}

pub fn extract_pack(
    mut reader: impl std::io::Read + std::io::Seek,
) -> Result<ExtractedPack, ExtractPackError> {
    // Save the stream position
    let initial_position = reader
        .stream_position()
        .map_err(ExtractPackError::ReadPackedProgramError)?;

    // Get the total length of the reader. We use `.seek()` instead of
    // `.stream_len()` to avoid unnecessary seeks
    let packed_len = reader
        .seek(SeekFrom::End(0))
        .map_err(ExtractPackError::ReadPackedProgramError)?;

    // Rewind to read the marker and length from the reader
    let end_marker_with_length_start = packed_len
        .checked_sub((MARKER.len() + LENGTH_BYTES).try_into()?)
        .ok_or(ExtractPackError::MarkerNotFound)?;
    reader
        .seek(SeekFrom::Start(end_marker_with_length_start))
        .map_err(ExtractPackError::ReadPackedProgramError)?;
    let mut end_marker_with_length = [0u8; MARKER.len() + LENGTH_BYTES];
    reader
        .read_exact(&mut end_marker_with_length)
        .map_err(ExtractPackError::ReadPackedProgramError)?;

    let (end_length_bytes, end_marker) = end_marker_with_length.split_at(LENGTH_BYTES);

    // Validate the marker matches the expected value
    if end_marker != MARKER {
        return Err(ExtractPackError::MarkerNotFound);
    }

    // Parse the length bytes
    let end_length_bytes: [u8; LENGTH_BYTES] = end_length_bytes
        .try_into()
        .map_err(|_| ExtractPackError::MalformedMarker)?;
    let end_length = LengthInt::from_le_bytes(end_length_bytes);
    let pack_length: usize = end_length.try_into()?;

    // Calculate where the pack starts and where the unpacked data ends
    let pack_start = end_marker_with_length_start
        .checked_sub(end_length.into())
        .ok_or(ExtractPackError::MalformedMarker)?;
    let unpacked_end = pack_start
        .checked_sub((MARKER.len() + LENGTH_BYTES).try_into()?)
        .ok_or(ExtractPackError::MalformedMarker)?;

    // Rewind to read the pack data plus the other marker / length into a vector
    reader
        .seek(SeekFrom::Start(unpacked_end))
        .map_err(ExtractPackError::ReadPackedProgramError)?;
    let mut pack_bytes_with_marker_and_length =
        vec![0u8; pack_length + MARKER.len() + LENGTH_BYTES];
    reader
        .read_exact(&mut pack_bytes_with_marker_and_length)
        .map_err(ExtractPackError::ReadPackedProgramError)?;

    // Separate the marker, length, and pack data
    let (start_marker_with_length, pack_bytes) =
        pack_bytes_with_marker_and_length.split_at_mut(MARKER.len() + LENGTH_BYTES);
    let (start_marker, start_length_bytes) = start_marker_with_length.split_at_mut(MARKER.len());

    // Validate the other marker and length match the original marker and length
    if start_marker != MARKER {
        return Err(ExtractPackError::MalformedMarker);
    }
    if end_length_bytes != start_length_bytes {
        return Err(ExtractPackError::MalformedMarker);
    }

    // Restore the stream position
    reader
        .seek(SeekFrom::Start(initial_position))
        .map_err(ExtractPackError::ReadPackedProgramError)?;

    // Deserialize the pack data
    let (pack, _) = bincode::decode_from_slice(pack_bytes, bincode::config::standard())
        .map_err(ExtractPackError::InvalidPack)?;

    // Return the extracted pack and the length of the unpacked data
    let unpacked_len = unpacked_end.try_into()?;
    Ok(ExtractedPack { pack, unpacked_len })
}

#[derive(Debug, thiserror::Error)]
pub enum InjectPackError {
    #[error("failed to write packed program: {0}")]
    IoError(#[from] std::io::Error),
    #[error("failed to serialize pack: {0}")]
    SerializeError(#[source] bincode::error::EncodeError),
    #[error("pack JSON too large")]
    PackTooLarge,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractPackError {
    #[error("failed to read packed program: {0}")]
    ReadPackedProgramError(#[source] std::io::Error),
    #[error("marker not found at end of the packed program")]
    MarkerNotFound,
    #[error("marker was malformed at the end of the packed program")]
    MalformedMarker,
    #[error("failed to parse pack: {0}")]
    InvalidPack(#[source] bincode::error::DecodeError),
    #[error(transparent)]
    TryFromIntError(#[from] std::num::TryFromIntError),
}
