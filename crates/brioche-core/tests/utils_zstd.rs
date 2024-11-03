use std::io::{Read as _, Seek as _, Write as _};

use assert_matches::assert_matches;
use brioche_core::utils::{
    io::NotSeekable,
    zstd::{ZstdLinearDecoder, ZstdSeekableDecoder, ZstdSeekableEncoder},
};
use proptest::prelude::*;
use tokio::io::AsyncWriteExt as _;

const MAX_DATA_SIZE: usize = 1024;

prop_compose! {
    fn arb_data()(data in prop::collection::vec(any::<u8>(), 0..=MAX_DATA_SIZE)) -> Vec<u8> {
        data
    }
}

prop_compose! {
    fn arb_data_with_cursor_position()(data in prop::collection::vec(any::<u8>(), 0..=MAX_DATA_SIZE))(index in 0..=data.len(), data in Just(data)) -> (Vec<u8>, usize) {
        (data, index)
    }
}

fn arb_encoded_data_with_cursor_position() -> impl Strategy<Value = (EncodedData, usize)> {
    prop::collection::vec(any::<u8>(), 0..=MAX_DATA_SIZE).prop_flat_map(|data| {
        let encoded_data = zstd::encode_all(&data[..], 0).unwrap();
        let encoded_len = encoded_data.len();
        let encoded_data = EncodedData {
            encoded: encoded_data,
            decoded: data,
        };
        (Just(encoded_data), 0..=encoded_len)
    })
}

fn arb_frames_with_cursor_position() -> impl Strategy<Value = (Vec<Vec<u8>>, usize)> {
    any::<Vec<Vec<u8>>>().prop_flat_map(|frames| {
        let total_len = frames.iter().map(|frame| frame.len()).sum::<usize>();
        (Just(frames), 0..=total_len)
    })
}

#[derive(Debug, Clone)]
struct EncodedData {
    encoded: Vec<u8>,
    decoded: Vec<u8>,
}

proptest! {
    #[test]
    fn test_utils_zstd_encode_seekable(data in arb_data(), level in 1usize..10, frame_size in 1..MAX_DATA_SIZE) {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
            let mut encoded = vec![];
            let mut encoder = ZstdSeekableEncoder::new(&mut encoded, level, frame_size).unwrap();
            encoder.write_all(&data).await.unwrap();
            encoder.shutdown().await.unwrap();

            let decoded = zstd::decode_all(&encoded[..]).unwrap();
            assert!(decoded == data, "decoded data does not match data");
        });
    }

    #[test]
    fn test_utils_zstd_decode_seekable(data in arb_data(), level in 1usize..10, frame_size in 1usize..1000*1000) {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
            let mut encoded = vec![];
            let mut encoder = ZstdSeekableEncoder::new(&mut encoded, level, frame_size).unwrap();
            encoder.write_all(&data).await.unwrap();
            encoder.shutdown().await.unwrap();

            let decoded = zstd::decode_all(&encoded[..]).unwrap();
            assert!(decoded == data, "decoded data does not match data");

            let mut seekable_decoder = ZstdSeekableDecoder::new(Box::new(std::io::Cursor::new(&encoded[..]))).unwrap();
            let mut seekable_decoded = vec![];
            seekable_decoder.read_to_end(&mut seekable_decoded).unwrap();
            assert!(seekable_decoded == data, "decoded data does not match data");
        });
    }

    #[test]
    fn test_utils_zstd_decode_seekable_seeking((data, pos) in arb_data_with_cursor_position(), level in 1usize..10, frame_size in 1usize..1000*1000) {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
            let mut encoded = vec![];
            let mut encoder = ZstdSeekableEncoder::new(&mut encoded, level, frame_size).unwrap();
            encoder.write_all(&data).await.unwrap();
            encoder.shutdown().await.unwrap();

            let decoded = zstd::decode_all(&encoded[..]).unwrap();
            assert!(decoded == data, "decoded data does not match data");

            let mut seekable_decoder = ZstdSeekableDecoder::new(Box::new(std::io::Cursor::new(&encoded[..]))).unwrap();

            let mut seekable_decoded = vec![];
            seekable_decoder.seek(std::io::SeekFrom::Start(pos as u64)).unwrap();
            seekable_decoder.read_to_end(&mut seekable_decoded).unwrap();
            assert!(seekable_decoded == data[pos..], "decoded data does not match data");

            seekable_decoded.clear();
            seekable_decoder.seek(std::io::SeekFrom::End(-(pos as i64))).unwrap();
            seekable_decoder.read_to_end(&mut seekable_decoded).unwrap();
            assert!(seekable_decoded == data[(data.len() - pos)..], "decoded data does not match data");

            seekable_decoded.clear();
            seekable_decoder.seek(std::io::SeekFrom::End(0)).unwrap();
            seekable_decoder.read_to_end(&mut seekable_decoded).unwrap();
            assert!(seekable_decoded.is_empty(), "expected to decode nothing when at end position");

            seekable_decoded.clear();
            seekable_decoder.seek(std::io::SeekFrom::Current(-(pos as i64))).unwrap();
            seekable_decoder.read_to_end(&mut seekable_decoded).unwrap();
            assert!(seekable_decoded == data[(data.len() - pos)..], "decoded data does not match data");
        });
    }

    #[test]
    fn test_utils_zstd_linear_decode_mixed_frames(frames in any::<Vec<Vec<u8>>>(), level in 1i32..10) {
        let mut encoded = vec![];
        let mut encoded_writer = std::io::Cursor::new(&mut encoded);
        for frame in &frames {
            let mut encoder = zstd::Encoder::new(&mut encoded_writer, level).unwrap().auto_finish();
            encoder.write_all(frame).unwrap();
        }

        let mut decoder = ZstdLinearDecoder::new(encoded.as_slice()).unwrap();
        let mut decoded = vec![];
        decoder.read_to_end(&mut decoded).unwrap();

        let combined_data = frames.iter().flatten().copied().collect::<Vec<_>>();
        assert!(decoded == combined_data, "decoded data does not match data");
    }

    #[test]
    fn test_utils_zstd_linear_decode_seekable(data in arb_data(), level in 1usize..10, frame_size in 1..MAX_DATA_SIZE) {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
            let mut encoded = vec![];
            let mut encoder = ZstdSeekableEncoder::new(&mut encoded, level, frame_size).unwrap();
            encoder.write_all(&data).await.unwrap();
            encoder.shutdown().await.unwrap();


            let mut decoder = ZstdLinearDecoder::new(encoded.as_slice()).unwrap();
            let mut decoded = vec![];
            decoder.read_to_end(&mut decoded).unwrap();

            assert!(decoded == data, "decoded data does not match data");
        });
    }

    #[test]
    fn test_utils_zstd_linear_decode_resume((data, pos) in arb_encoded_data_with_cursor_position()) {
        let (first, second) = data.encoded.split_at(pos);

        let mut decoder = ZstdLinearDecoder::new(first).unwrap();
        let mut decoded = vec![];
        let _ = decoder.read_to_end(&mut decoded);
        assert!(decoder.reader().is_empty());
        *decoder.reader_mut() = second;
        decoder.read_to_end(&mut decoded).unwrap();

        assert!(decoded == data.decoded, "decoded data does not match data");
    }

    #[test]
    fn test_utils_zstd_linear_decode_seek((frames, pos) in arb_frames_with_cursor_position()) {
        let mut encoded = vec![];
        for frame in &frames {
            let mut encoder = zstd::Encoder::new(&mut encoded, 0).unwrap().auto_finish();
            encoder.write_all(frame).unwrap();
        }

        let data: Vec<u8> = frames.iter().flatten().copied().collect::<Vec<_>>();

        let mut decoded = vec![];
        let mut decoder = ZstdLinearDecoder::new(std::io::Cursor::new(&encoded)).unwrap();
        let cursor = decoder.seek(std::io::SeekFrom::Start(pos as u64)).unwrap();
        let _ = decoder.read_to_end(&mut decoded);
        assert_eq!(cursor, pos as u64);
        assert!(decoded == data[pos..], "decoded data does not match data");

        decoded.clear();
        let cursor = decoder.seek(std::io::SeekFrom::Start(0)).unwrap();
        let _ = decoder.read_to_end(&mut decoded);
        assert_eq!(cursor, 0);
        assert!(decoded == data, "decoded data does not match data");

        decoded.clear();
        let cursor = decoder.seek(std::io::SeekFrom::Current(-(pos as i64))).unwrap();
        let _ = decoder.read_to_end(&mut decoded);
        assert_eq!(cursor, (data.len() - pos) as u64);
        assert!(decoded == data[(data.len() - pos)..], "decoded data does not match data");

        decoded.clear();
        let cursor = decoder.seek(std::io::SeekFrom::End(-(pos as i64))).unwrap();
        let _ = decoder.read_to_end(&mut decoded);
        assert_eq!(cursor, (data.len() - pos) as u64);
        assert!(decoded == data[(data.len() - pos)..], "decoded data does not match data");

        decoded.clear();
        let mut decoder = ZstdLinearDecoder::new(std::io::Cursor::new(&encoded)).unwrap();
        let cursor = decoder.seek(std::io::SeekFrom::End(-(pos as i64))).unwrap();
        let _ = decoder.read_to_end(&mut decoded);
        assert_eq!(cursor, (data.len() - pos) as u64);
        assert!(decoded == data[(data.len() - pos)..], "decoded data does not match data");
    }

    #[test]
    fn test_utils_zstd_linear_decode_seek_with_non_seekable((frames, pos) in arb_frames_with_cursor_position()) {
        let data: Vec<u8> = frames.iter().flatten().copied().collect::<Vec<_>>();
        prop_assume!((0..data.len()).contains(&pos));

        let mut encoded = vec![];
        for frame in &frames {
            let mut encoder = zstd::Encoder::new(&mut encoded, 0).unwrap().auto_finish();
            encoder.write_all(frame).unwrap();
        }

        let mut next_frame_start = 0usize;
        let mut frames_iter = frames.iter();
        let frame_start;
        let frame_end;
        loop {
            let frame = frames_iter.next().unwrap();
            let start = next_frame_start;
            next_frame_start += frame.len();

            if (start..next_frame_start).contains(&pos) {
                frame_start = start;
                frame_end = next_frame_start;
                break;
            }
        }

        let mut decoder = ZstdLinearDecoder::new(NotSeekable(&encoded[..])).unwrap();
        let cursor = decoder.seek(std::io::SeekFrom::Start(frame_end as u64)).unwrap();
        assert_eq!(cursor, frame_end as u64);

        let cursor = decoder.seek(std::io::SeekFrom::Start(pos as u64)).unwrap();
        assert_eq!(cursor, pos as u64);

        let cursor = decoder.seek(std::io::SeekFrom::Start(frame_start as u64)).unwrap();
        assert_eq!(cursor, frame_start as u64);

        let mut decoded = vec![];
        let _ = decoder.read_to_end(&mut decoded);
        assert!(decoded == data[frame_start..], "decoded data does not match data");

        let past_frame_end = Some(frame_end + 1).filter(|pos| *pos <= data.len());
        if let Some(past_frame_end) = past_frame_end {
            let mut decoder = ZstdLinearDecoder::new(NotSeekable(&encoded[..])).unwrap();
            let cursor = decoder.seek(std::io::SeekFrom::Start(past_frame_end as u64)).unwrap();
            assert_eq!(cursor, past_frame_end as u64);

            let result = decoder.seek(std::io::SeekFrom::Start(pos as u64));
            assert_matches!(result, Err(_));
        }
    }
}

#[tokio::test]
async fn test_utils_zstd_decode_seekable_non_seekable_file_error() {
    let data = (0..1024u16)
        .flat_map(|i| i.to_le_bytes())
        .collect::<Vec<u8>>();
    let encoded = zstd::encode_all(&data[..], 3).unwrap();

    let result = ZstdSeekableDecoder::new(Box::new(std::io::Cursor::new(&encoded[..]))).map(|_| ());
    assert_matches!(result, Err(_));
}
