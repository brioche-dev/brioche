use std::{
    io::BufRead as _,
    ops::Range,
    pin::Pin,
    sync::OnceLock,
    task::{ready, Poll},
};

use anyhow::Context as _;
use zstd::stream::raw::Operation as _;

pin_project_lite::pin_project! {
    pub struct ZstdSeekableEncoder<W> {
        #[pin]
        writer: W,
        cstream: zstd_seekable::SeekableCStream,
        buffer: Vec<u8>,
        unwritten_start: usize,
        unwritten_end: usize,
        did_write_end: bool,
    }
}

/// Compress written data to an underlying writer with zstd. Data is
/// is chunked into multiple zstd frames, and a seek table is written
/// when the writer is shut down. The resulting decompressed with any
/// zstd decoder, but allows efficient seeking through the stream when
/// using [`ZstdSeekableDecoder`].
impl<W> ZstdSeekableEncoder<W> {
    pub fn new(writer: W, level: usize, frame_size: usize) -> Result<Self, zstd_seekable::Error> {
        let cstream = zstd_seekable::SeekableCStream::new(level, frame_size)?;
        Ok(Self {
            writer,
            cstream,
            buffer: vec![0; zstd_seekable::CStream::out_size()],
            unwritten_start: 0,
            unwritten_end: 0,
            did_write_end: false,
        })
    }

    /// Write any unwritten data to the underlying writer. Returns
    /// `Poll::Ready(Ok())` if all unwritten data has been flushed.
    fn flush_pending(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>>
    where
        W: tokio::io::AsyncWrite,
    {
        let mut this = self.project();

        loop {
            let pending = &this.buffer[*this.unwritten_start..*this.unwritten_end];

            if pending.is_empty() {
                *this.unwritten_start = 0;
                *this.unwritten_end = 0;

                return Poll::Ready(Ok(()));
            }

            let written = ready!(this.writer.as_mut().poll_write(cx, pending))?;
            if written == 0 {
                return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()));
            }

            *this.unwritten_start += written;
        }
    }
}

impl<W> tokio::io::AsyncWrite for ZstdSeekableEncoder<W>
where
    W: tokio::io::AsyncWrite,
{
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        static ZSTD_CSTREAM_IN_SIZE: OnceLock<usize> = OnceLock::new();
        let in_size = *ZSTD_CSTREAM_IN_SIZE.get_or_init(zstd_seekable::CStream::in_size);

        // Flush any pending data first
        ready!(self.as_mut().flush_pending(cx))?;

        let this = self.project();

        let read_len = buf.len().min(in_size);

        // Compress some data from the input buffer
        let (out_pos, in_pos) = this
            .cstream
            .compress(this.buffer, &buf[..read_len])
            .map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::Other, format!("zstd error: {error}"))
            })?;

        *this.unwritten_end = out_pos;

        Poll::Ready(Ok(in_pos))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.flush_pending(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        // Flush any pending data first
        ready!(self.as_mut().flush_pending(cx))?;

        let this = self.as_mut().project();

        if !*this.did_write_end {
            // If we haven't ended the stream yet, then we need to call
            // `.end_stream()` on the underlying encoder in order to
            // write the seekable table

            // End the stream
            let out_pos = this.cstream.end_stream(this.buffer).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::Other, format!("zstd error: {error}"))
            })?;

            // Even if we flushed already, we now have some more data
            // to flush
            *this.unwritten_end = out_pos;
            *this.did_write_end = true;

            // Try flushing the pending data again
            ready!(self.as_mut().flush_pending(cx))?;
        }

        // If we've reached this point, then we've written all pending data
        // to the underlying writer, including the seekable table

        let this = self.project();

        // Replace the buffer with an empty buffer. This effectively closes
        // the writer, since any attempts to write will short-circuit due
        // to not having anywhere for the encoded data to go
        *this.buffer = Vec::new();

        // Ensure the underlying writer shuts down
        this.writer.poll_shutdown(cx)
    }
}

pub struct ZstdSeekableDecoder<'a, R> {
    seekable: zstd_seekable::Seekable<'a, R>,
    pos: u64,
    decompressed_length: Option<u64>,
}

/// Decompress zstd-compressed data from a reader that has a seek table
/// at the end of the stream, such as one created with [`ZstdSeekableEncoder`].
/// Creation will fail if the reader doesn't have a seek table at the end.
impl<'a, R> ZstdSeekableDecoder<'a, R> {
    pub fn new(reader: Box<R>) -> Result<Self, zstd_seekable::Error>
    where
        R: std::io::Read + std::io::Seek,
    {
        let seekable = zstd_seekable::Seekable::init(reader)?;
        Ok(Self {
            seekable,
            pos: 0,
            decompressed_length: None,
        })
    }

    /// Get the total length of the decompressed stream.
    pub fn decompressed_len(&mut self) -> anyhow::Result<u64> {
        match self.decompressed_length {
            Some(decompressed_length) => Ok(decompressed_length),
            None => {
                let num_frames = self.seekable.get_num_frames();

                let last_frame_index = num_frames.checked_sub(1);
                let decompressed_length = last_frame_index
                    .map(|last_frame_index| {
                        let last_frame_offset = self
                            .seekable
                            .get_frame_decompressed_offset(last_frame_index);
                        let last_frame_size =
                            self.seekable.get_frame_decompressed_size(last_frame_index);
                        let last_frame_size: u64 = last_frame_size.try_into()?;

                        let decompressed_length = last_frame_offset
                            .checked_add(last_frame_size)
                            .context("invalid zstd decompressed length")?;

                        anyhow::Ok(decompressed_length)
                    })
                    .transpose()?;
                let decompressed_length = decompressed_length.unwrap_or(0);

                self.decompressed_length = Some(decompressed_length);
                Ok(decompressed_length)
            }
        }
    }
}

impl<'a, R> std::io::Read for ZstdSeekableDecoder<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        // Decompress data into the provided buffer, bsaed on the current
        // decoder's position
        let result = self
            .seekable
            .decompress(buf, self.pos)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        // Move the cursor based on how much dta was decoded
        let result_u64: u64 = result.try_into().expect("zstd cursor out of range");
        self.pos = self
            .pos
            .checked_add(result_u64)
            .expect("zstd cursor out of range");

        Ok(result)
    }
}

impl<'a, R> std::io::Seek for ZstdSeekableDecoder<'a, R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match pos {
            std::io::SeekFrom::Start(offset) => {
                self.pos = offset;
                Ok(self.pos)
            }
            std::io::SeekFrom::End(offset) => {
                let decompressed_length = self.decompressed_len().map_err(std::io::Error::other)?;

                let pos: u64 = decompressed_length
                    .checked_add_signed(offset)
                    .ok_or_else(|| std::io::Error::other("tried to seek out of range"))?;
                self.pos = pos;
                Ok(self.pos)
            }
            std::io::SeekFrom::Current(offset) => {
                self.pos = self
                    .pos
                    .checked_add_signed(offset)
                    .expect("tried to seek out-of-range");
                Ok(self.pos)
            }
        }
    }
}

/// Decompress zstd-compressed data from a reader, buffering the current
/// zstd frame.
///
/// This type keeps a buffer of the current decompressed frame, and
/// additionally keeps indices for each fully-decompressed frame. For a zstd
/// stream containing only one frame, this will effectively buffer the entire
/// decompressed stream into memory, but for streams containing multiple
/// frames (such as a stream produced by [`ZstdSeekableEncoder`]), this type
/// offers a few unique features:
///
/// - Seeking backwards to previous frames efficiently.
/// - Seeking within the current frame even if the underlying type doesn't
///   support seeking (by using [`crate::utils::io::NotSeekable`]).
/// - Resuming an interrupted stream, allowing for `tail -f` style behavior.
///
/// This type supports both seeking forwards and backwards. As long as the
/// target of the seek operation is within the current frame, then the
/// underlying type can use [`crate::utils::io::NotSeekable`] (which will
/// error out when trying to seek to a past frame). Seeking relative to the
/// end will need to decompress the entire stream first.
pub struct ZstdLinearDecoder<R> {
    reader: R,
    frames: Vec<ZstdFrameInfo>,
    current_frame: ZstdCurrentFrame,
    input_buffer: Vec<u8>,
    input_tail: usize,
}

impl<R> ZstdLinearDecoder<R> {
    pub fn new(reader: R) -> anyhow::Result<Self> {
        let input_buffer_size = zstd::zstd_safe::CCtx::in_size();
        let input_buffer = vec![0; input_buffer_size];
        let current_frame = ZstdCurrentFrame::initial_frame()?;

        Ok(Self {
            reader,
            frames: vec![],
            current_frame,
            input_buffer,
            input_tail: 0,
        })
    }

    pub fn reader(&self) -> &R {
        &self.reader
    }

    pub fn reader_mut(&mut self) -> &mut R {
        &mut self.reader
    }

    /// Decode the stream until reaching the end. Returns the offset from
    /// the start of the stream.
    fn jump_to_end(&mut self) -> anyhow::Result<u64>
    where
        R: std::io::Read,
    {
        let mut pos = self.current_frame.decompressed_pos();

        loop {
            // Decode some more bytes
            let filled_buf = self.fill_buf()?;

            // If we can't decode any more bytes, that means we're at the
            // end
            if filled_buf.is_empty() {
                break;
            }

            // Consume the decoded bytes
            let filled_len = filled_buf.len();
            let filled_len_u64: u64 = filled_len.try_into()?;
            pos += filled_len_u64;
            self.consume(filled_len);
        }

        Ok(pos)
    }

    /// Seek into the latest frame. If the current frame is the latest, then
    /// this is a no-op. Returns the offset from the start of the stream.
    fn seek_into_latest_frame(&mut self) -> anyhow::Result<u64>
    where
        R: std::io::Seek,
    {
        if self.frames.len() == self.current_frame.frame_index {
            // Current frame is already the latest
            return Ok(self.current_frame.decompressed_pos());
        }

        // Get the latest frame
        let latest_frame_index = self
            .frames
            .len()
            .checked_sub(1)
            .expect("frame list is empty but current frame is not the latest");
        let latest_frame = &self.frames[latest_frame_index];

        // "Clear" the input buffer (by restting the tail offset)
        self.input_tail = 0;

        // Seek to the compressed offset to the frame
        self.reader.seek(std::io::SeekFrom::Start(
            latest_frame.compressed_range.start,
        ))?;

        // Reset the internal decoder and offsets
        let pos = self
            .current_frame
            .seeked_to(latest_frame_index, latest_frame)?;

        Ok(pos)
    }

    /// Seek into a previously-decoded frame by the frame index. If the
    /// current frame is already decoding this frame, then this is a no-op.
    /// Returns the offset from the start of the stream.
    fn seek_into_frame(&mut self, frame_index: usize) -> anyhow::Result<u64>
    where
        R: std::io::Seek,
    {
        if self.current_frame.frame_index == frame_index {
            // Current frame is already the target frame
            return Ok(self.current_frame.decompressed_pos());
        }

        // Get the target frame
        let frame = self
            .frames
            .get(frame_index)
            .context("frame index out of range")?;

        // "Clear" the input buffer (by restting the tail offset)
        self.input_tail = 0;

        // Seek to the compressed offset to the frame
        self.reader
            .seek(std::io::SeekFrom::Start(frame.compressed_range.start))?;

        // Reset the internal decoder and offsets
        let pos = self.current_frame.seeked_to(frame_index, frame)?;

        Ok(pos)
    }
}

impl<R> std::io::Read for ZstdLinearDecoder<R>
where
    R: std::io::Read,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let filled = self.fill_buf()?;
        let consumed = filled.len().min(buf.len());

        buf[..consumed].copy_from_slice(&filled[..consumed]);
        self.consume(consumed);
        Ok(consumed)
    }
}

impl<R> std::io::BufRead for ZstdLinearDecoder<R>
where
    R: std::io::Read,
{
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        // Loop until we've got some decoded data to return (or until the
        // underlying reader hits EOF)
        loop {
            // If we have some buffered decoded data that hasn't been
            // consumed, return that first.
            let unread = self.current_frame.unread().map_err(std::io::Error::other)?;
            if !unread.is_empty() {
                // HACK: This should be `return Ok(unread)` but that
                // causes a borrow checker error
                break;
            }

            match self.current_frame.state {
                ZstdCurrentFrameState::Finished => {
                    // Current frame is finished, so update the internal
                    // decoder to start the next frame
                    let frame_index = self.current_frame.frame_index;
                    let frame_info = self.current_frame.finish().map_err(std::io::Error::other)?;

                    // Save the info for the finished frame if it's new
                    if self.frames.get(frame_index).is_none() {
                        self.frames.insert(frame_index, frame_info);
                    }
                }
                ZstdCurrentFrameState::Failed => {
                    // Decoding failed. Conservatively, we return an
                    // error since the underlying zstd decoder could be
                    // in a weird state
                    return Err(std::io::Error::other("zstd decoder failed while decoding"))?;
                }
                ZstdCurrentFrameState::InProgress => {
                    // Get the buffered input that hasn't been decoded yet
                    let mut input = &self.input_buffer[..self.input_tail];

                    if input.is_empty() {
                        // We don't have any undecoded input, so read some
                        // more from the underlying reader
                        let input_tail = &mut self.input_buffer[self.input_tail..];
                        let input_read_length = self.reader.read(input_tail)?;
                        self.input_tail += input_read_length;
                        input = &self.input_buffer[..self.input_tail];
                    }

                    if input.is_empty() {
                        // Still no data to decode, which means we've decoded
                        // everything and the underlying reader doesn't have
                        // any more data

                        if self.current_frame.decompressed.is_empty() {
                            // If we're at the start of a new frame, then
                            // we're done
                            break;
                        } else {
                            // We don't have any more data but the underlying
                            // decoder isn't finished, this is an error! We
                            // don't need to transition to the "failed" state
                            // since we can just resume decoding if the
                            // underlying reader gets more data
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "unexpected end of input while decoding zstd stream",
                            ));
                        }
                    }

                    // Decode from the (buffered) input data
                    let mut decode_in = zstd::stream::raw::InBuffer::around(input);

                    // Reserve more space in the decompressed data buffer for
                    // the decoder's output
                    let decompressed_reserved = zstd::zstd_safe::CCtx::out_size();
                    self.current_frame
                        .decompressed
                        .reserve(decompressed_reserved);

                    // Decode into the decompressed buffer
                    let decompressed_start_pos = self.current_frame.decompressed.len();
                    let mut decode_out = zstd::stream::raw::OutBuffer::around_pos(
                        &mut self.current_frame.decompressed,
                        decompressed_start_pos,
                    );

                    // Decode! This is where we do the actual decompression!
                    let result = self
                        .current_frame
                        .decoder
                        .run(&mut decode_in, &mut decode_out);

                    // "Slide" the input buffer over, so the start is the
                    // data that the decoder hasn't consumed yet
                    let input_head = decode_in.pos();
                    self.input_buffer
                        .copy_within(input_head..self.input_tail, 0);
                    self.input_tail -= input_head;

                    // Track how much compressed data we've consumed so far
                    let input_head_u64: u64 =
                        input_head.try_into().map_err(std::io::Error::other)?;
                    self.current_frame.compressed_length += input_head_u64;

                    match result {
                        Ok(0) => {
                            // Decoder indicated that we finished the
                            // current frame
                            self.current_frame.state = ZstdCurrentFrameState::Finished;
                        }
                        Ok(_) => {}
                        Err(_) => {
                            // Decoder failed!
                            self.current_frame.state = ZstdCurrentFrameState::Failed;
                        }
                    }

                    // If we decoded any data, then we've got some buffered
                    // data that we can now return, so we're done
                    let newly_decompressed = self
                        .current_frame
                        .decompressed
                        .len()
                        .saturating_sub(decompressed_start_pos);
                    if newly_decompressed > 0 {
                        break;
                    }
                }
            }
        }

        // Return any buffered data that hasn't been consumed yet, if any
        let unread = self.current_frame.unread().map_err(std::io::Error::other)?;
        Ok(unread)
    }

    fn consume(&mut self, amt: usize) {
        let amt: u64 = amt.try_into().expect("amt out of range");
        let new_frame_offset = self
            .current_frame
            .frame_pos
            .checked_add(amt)
            .expect("frame offset out of range");
        let decompressed_len: u64 = self
            .current_frame
            .decompressed
            .len()
            .try_into()
            .expect("decompressed.len() out of range");
        assert!(new_frame_offset <= decompressed_len);

        self.current_frame.frame_pos = new_frame_offset;
    }
}

impl<R> std::io::Seek for ZstdLinearDecoder<R>
where
    R: std::io::Read + std::io::Seek,
{
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        // Get the target position relative to the start of the stream
        let target_pos = match pos {
            std::io::SeekFrom::Start(offset) => offset,
            std::io::SeekFrom::Current(offset) => {
                let current_pos = self.current_frame.decompressed_pos();
                current_pos
                    .checked_add_signed(offset)
                    .ok_or_else(|| std::io::Error::other("seek out of range"))?
            }
            std::io::SeekFrom::End(offset) => {
                // Seek to the latest frame, if we're not already in it.
                // This only seeks the underlying reader if we're not in the
                // latest frame
                self.seek_into_latest_frame()
                    .map_err(std::io::Error::other)?;

                // Decode the entire stream until we reach the end
                let end_pos = self.jump_to_end().map_err(std::io::Error::other)?;

                end_pos
                    .checked_add_signed(offset)
                    .ok_or_else(|| std::io::Error::other("seek out of range"))?
            }
        };

        // Get the start and end offsets of the currently decoded frame
        let current_frame_start = self.current_frame.decompressed_start_pos;
        let current_frame_loaded_length: u64 = self
            .current_frame
            .decompressed
            .len()
            .try_into()
            .map_err(std::io::Error::other)?;
        let current_frame_end = current_frame_start + current_frame_loaded_length;

        if (current_frame_start..current_frame_end).contains(&target_pos) {
            // Target position is within the current frame, so all we need
            // to do is adjust the current frame's position

            let new_frame_pos = target_pos - current_frame_start;
            self.current_frame.frame_pos = new_frame_pos;

            Ok(target_pos)
        } else {
            // Find the frame containing the target position
            let matched_frame = self
                .frames
                .iter()
                .enumerate()
                .find(|(_, info)| info.decompressed_range.contains(&target_pos));

            let mut current_pos = match matched_frame {
                Some((frame_index, _)) => {
                    // If we know a previous frame covers the target position,
                    // seek to the start of it
                    self.seek_into_frame(frame_index)
                        .map_err(std::io::Error::other)?
                }
                None => {
                    // If we didn't find a match, then we know the target
                    // position is somewhere past the end of what we've
                    // decoded already. So, seek to the start of the last
                    // frame we know about.
                    self.seek_into_latest_frame()
                        .map_err(std::io::Error::other)?
                }
            };
            assert!(current_pos <= target_pos);

            // Repeatedly decode until we reach the target position
            while current_pos < target_pos {
                // Decode some data
                let filled_buf = self.fill_buf()?;

                if filled_buf.is_empty() {
                    // If we couldn't do anything, then we couldn't seek
                    // to the target position, so return an error

                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        format!("unexpected EOF while seeking to position {target_pos}"),
                    ));
                }

                // Consume the decoded data (up until we reach the
                // target position)
                let filled_buf_len = filled_buf.len();
                let filled_buf_len_u64: u64 =
                    filled_buf_len.try_into().map_err(std::io::Error::other)?;
                let consumed = filled_buf_len_u64.min(target_pos - current_pos);
                let consumed_usize: usize = consumed.try_into().map_err(std::io::Error::other)?;

                self.consume(consumed_usize);
                current_pos += consumed;
            }

            assert_eq!(current_pos, target_pos);
            Ok(target_pos)
        }
    }
}

#[derive(Debug)]
struct ZstdFrameInfo {
    compressed_range: Range<u64>,
    decompressed_range: Range<u64>,
}

struct ZstdCurrentFrame {
    decoder: zstd::stream::raw::Decoder<'static>,
    frame_index: usize,
    frame_pos: u64,
    compressed_start_pos: u64,
    decompressed_start_pos: u64,
    compressed_length: u64,
    decompressed: Vec<u8>,
    state: ZstdCurrentFrameState,
}

impl ZstdCurrentFrame {
    fn initial_frame() -> anyhow::Result<Self> {
        let decoder = zstd::stream::raw::Decoder::new()?;

        Ok(Self {
            decoder,
            frame_index: 0,
            frame_pos: 0,
            compressed_start_pos: 0,
            compressed_length: 0,
            decompressed_start_pos: 0,
            decompressed: vec![],
            state: ZstdCurrentFrameState::InProgress,
        })
    }

    fn unread(&self) -> anyhow::Result<&[u8]> {
        let cursor_offset: usize = self.frame_pos.try_into()?;
        let unread = &self.decompressed[cursor_offset..];
        Ok(unread)
    }

    /// Return the current offset relative to the start of the stream
    fn decompressed_pos(&self) -> u64 {
        self.decompressed_start_pos + self.frame_pos
    }

    /// Finish the frame to prepare to decode the next frame, clearing any internal state. Returns info for the just-finished frame.
    fn finish(&mut self) -> anyhow::Result<ZstdFrameInfo> {
        anyhow::ensure!(
            matches!(self.state, ZstdCurrentFrameState::Finished),
            "called .finish(), but frame is not yet finished"
        );

        let decompressed_len: u64 = self.decompressed.len().try_into()?;

        let compressed_start = self.compressed_start_pos;
        let compressed_end = compressed_start + self.compressed_length;
        let decompressed_start = self.decompressed_start_pos;
        let decompressed_end = decompressed_start + decompressed_len;

        let compressed_range = compressed_start..compressed_end;
        let decompressed_range = decompressed_start..decompressed_end;

        let Self {
            decoder,
            frame_index,
            frame_pos: cursor_decompressed_frame_offset,
            compressed_start_pos: compressed_offset,
            decompressed_start_pos: decompressed_offset,
            compressed_length,
            decompressed,
            state,
        } = self;

        decoder.reinit()?;
        *frame_index += 1;
        *cursor_decompressed_frame_offset = 0;
        *compressed_offset = compressed_end;
        *compressed_length = 0;
        *decompressed_offset = decompressed_end;
        decompressed.clear();
        *state = ZstdCurrentFrameState::InProgress;

        Ok(ZstdFrameInfo {
            compressed_range,
            decompressed_range,
        })
    }

    /// Indicates that the underlying reader has been reset to the start
    /// of a frame. Resets internal state to prepare for decoding the frame.
    fn seeked_to(
        &mut self,
        new_frame_index: usize,
        frame_info: &ZstdFrameInfo,
    ) -> anyhow::Result<u64> {
        let Self {
            decoder,
            frame_index,
            frame_pos: cursor_decompressed_frame_offset,
            compressed_start_pos: compressed_offset,
            decompressed_start_pos: decompressed_offset,
            compressed_length,
            decompressed,
            state,
        } = self;

        decoder.reinit()?;
        *frame_index = new_frame_index;
        *cursor_decompressed_frame_offset = 0;
        *compressed_offset = frame_info.compressed_range.start;
        *compressed_length = 0;
        *decompressed_offset = frame_info.decompressed_range.start;
        decompressed.clear();
        *state = ZstdCurrentFrameState::InProgress;

        Ok(frame_info.decompressed_range.start)
    }
}

#[derive(Debug, Clone, Copy)]
enum ZstdCurrentFrameState {
    InProgress,
    Finished,
    Failed,
}
