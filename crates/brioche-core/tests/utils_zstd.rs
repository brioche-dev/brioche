use std::io::{Read as _, Seek as _, Write as _};

use assert_matches::assert_matches;
use brioche_core::utils::zstd::{ZstdLinearDecoder, ZstdSeekableDecoder, ZstdSeekableEncoder};
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
    fn test_utils_zstd_linear_decode_resume(data in arb_data(), level in 1i32..10, split_at in 1..(MAX_DATA_SIZE * 2)) {
        let encoded = zstd::encode_all(&data[..], level).unwrap();

        let split_at = split_at.min(encoded.len());
        let (first, second) = encoded.split_at(split_at);

        let mut decoder = ZstdLinearDecoder::new(first).unwrap();
        let mut decoded = vec![];
        let _ = decoder.read_to_end(&mut decoded);
        assert!(decoder.reader().is_empty());
        *decoder.reader_mut() = second;
        decoder.read_to_end(&mut decoded).unwrap();

        assert!(decoded == data, "decoded data does not match data");
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
