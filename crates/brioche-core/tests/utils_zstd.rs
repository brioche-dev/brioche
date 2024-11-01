use std::io::{Read as _, Seek as _};

use brioche_core::utils::zstd::{ZstdSeekableDecoder, ZstdSeekableEncoder};
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
}
