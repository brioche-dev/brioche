use std::{
    io::{BufRead as _, Read as _},
    ops::Range,
    pin::Pin,
    sync::OnceLock,
    task::{ready, Poll},
};

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

        ready!(self.as_mut().flush_pending(cx))?;

        let this = self.project();

        let read_len = buf.len().min(in_size);

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
        ready!(self.as_mut().flush_pending(cx))?;

        let this = self.as_mut().project();

        if !*this.did_write_end {
            let out_pos = this.cstream.end_stream(this.buffer).map_err(|error| {
                std::io::Error::new(std::io::ErrorKind::Other, format!("zstd error: {error}"))
            })?;

            *this.unwritten_end = out_pos;
            *this.did_write_end = true;

            ready!(self.as_mut().flush_pending(cx))?;
        }

        let this = self.project();

        // Take the buffer to drop it. This will effectively close the
        // writer, since writes won't have anywhere to go to
        std::mem::take(this.buffer);

        this.writer.poll_shutdown(cx)
    }
}

pub struct ZstdSeekableDecoder<'a, R> {
    seekable: zstd_seekable::Seekable<'a, R>,
    cursor: u64,
    decompressed_length: Option<u64>,
}

impl<'a, R> ZstdSeekableDecoder<'a, R> {
    pub fn new(reader: Box<R>) -> Result<Self, zstd_seekable::Error>
    where
        R: std::io::Read + std::io::Seek,
    {
        let seekable = zstd_seekable::Seekable::init(reader)?;
        Ok(Self {
            seekable,
            cursor: 0,
            decompressed_length: None,
        })
    }
}

impl<'a, R> std::io::Read for ZstdSeekableDecoder<'a, R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let result = self
            .seekable
            .decompress(buf, self.cursor)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;

        let result_u64: u64 = result.try_into().expect("zstd cursor out of range");
        self.cursor = self
            .cursor
            .checked_add(result_u64)
            .expect("zstd cursor out of range");

        Ok(result)
    }
}

impl<'a, R> std::io::Seek for ZstdSeekableDecoder<'a, R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match pos {
            std::io::SeekFrom::Start(offset) => {
                self.cursor = offset;
                Ok(self.cursor)
            }
            std::io::SeekFrom::End(offset) => {
                let decompressed_length = match self.decompressed_length {
                    Some(decompressed_length) => decompressed_length,
                    None => {
                        let num_frames = dbg!(self.seekable.get_num_frames());

                        let last_frame_index = num_frames.checked_sub(1);
                        let decompressed_length = last_frame_index
                            .map(|last_frame_index| {
                                let last_frame_offset = dbg!(self
                                    .seekable
                                    .get_frame_decompressed_offset(last_frame_index));
                                let last_frame_size = dbg!(self
                                    .seekable
                                    .get_frame_decompressed_size(last_frame_index));
                                let last_frame_size: u64 =
                                    dbg!(last_frame_size.try_into().map_err(|_| {
                                        std::io::Error::other("invalid zstd frame size")
                                    })?);

                                let decompressed_length = last_frame_offset
                                    .checked_add(last_frame_size)
                                    .ok_or_else(|| {
                                        std::io::Error::other("invalid zstd decompressed length")
                                    })?;

                                std::io::Result::Ok(decompressed_length)
                            })
                            .transpose()?;
                        let decompressed_length = decompressed_length.unwrap_or(0);

                        self.decompressed_length = Some(decompressed_length);
                        decompressed_length
                    }
                };

                let cursor: u64 = decompressed_length
                    .checked_add_signed(offset)
                    .ok_or_else(|| std::io::Error::other("tried to seek out of range"))?;
                self.cursor = dbg!(cursor);
                Ok(self.cursor)
            }
            std::io::SeekFrom::Current(offset) => {
                self.cursor = self
                    .cursor
                    .checked_add_signed(offset)
                    .expect("tried to seek out-of-range");
                Ok(self.cursor)
            }
        }
    }
}

pub struct ZstdLinearDecoder<R> {
    reader: R,
    frames: Vec<ZstdFrameInfo>,
    current_frame: ZstdCurrentFrame,
    input_buffer: Vec<u8>,
    input_tail: usize,
    output_buffer: Vec<u8>,
}

impl<R> ZstdLinearDecoder<R> {
    pub fn new(reader: R) -> anyhow::Result<Self> {
        let input_buffer_size = zstd::zstd_safe::CCtx::in_size();
        let input_buffer = vec![0; input_buffer_size];
        let output_buffer_size = zstd::zstd_safe::DCtx::out_size();
        let output_buffer = Vec::with_capacity(output_buffer_size);
        let current_frame = ZstdCurrentFrame::initial_frame()?;

        Ok(Self {
            reader,
            frames: vec![],
            current_frame,
            input_buffer,
            input_tail: 0,
            output_buffer,
        })
    }

    pub fn reader(&self) -> &R {
        &self.reader
    }

    pub fn reader_mut(&mut self) -> &mut R {
        &mut self.reader
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
        loop {
            let unread = self.current_frame.unread().map_err(std::io::Error::other)?;
            if !unread.is_empty() {
                // HACK: This should be `return Ok(unread)` but that
                // causes a borrow checker error
                break;
            }

            match self.current_frame.state {
                ZstdCurrentFrameState::Finished => {
                    let frame_index = self.current_frame.frame_index;
                    let frame_info = self.current_frame.finish().map_err(std::io::Error::other)?;

                    if self.frames.get(frame_index).is_none() {
                        self.frames.insert(frame_index, frame_info);
                    }
                }
                ZstdCurrentFrameState::Failed => {
                    return Err(std::io::Error::other("zstd decoder failed while decoding"))?;
                }
                ZstdCurrentFrameState::InProgress => {
                    let mut input = &self.input_buffer[..self.input_tail];
                    if input.is_empty() {
                        let input_tail = &mut self.input_buffer[self.input_tail..];
                        let input_read_length = self.reader.read(input_tail)?;
                        self.input_tail += input_read_length;
                        input = &self.input_buffer[..self.input_tail];
                    }

                    if input.is_empty() {
                        if self.current_frame.decompressed.is_empty() {
                            break;
                        } else {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "unexpected end of input while decoding zstd",
                            ));
                        }
                    }
                    self.output_buffer.clear();

                    let mut decode_in = zstd::stream::raw::InBuffer::around(input);
                    let mut decode_out =
                        zstd::stream::raw::OutBuffer::around(&mut self.output_buffer);
                    let result = self
                        .current_frame
                        .decoder
                        .run(&mut decode_in, &mut decode_out);

                    let decompressed_length = decode_out.as_slice().len();
                    self.current_frame
                        .decompressed
                        .extend_from_slice(decode_out.as_slice());

                    let input_head = decode_in.pos();
                    self.input_buffer
                        .copy_within(input_head..self.input_tail, 0);
                    self.input_tail -= input_head;

                    match result {
                        Ok(0) => {
                            self.current_frame.state = ZstdCurrentFrameState::Finished;
                        }
                        Ok(_) => {}
                        Err(_) => {
                            self.current_frame.state = ZstdCurrentFrameState::Failed;
                        }
                    }

                    if decompressed_length > 0 {
                        break;
                    }
                }
            }
        }

        let unread = self.current_frame.unread().map_err(std::io::Error::other)?;
        Ok(unread)
    }

    fn consume(&mut self, amt: usize) {
        let amt: u64 = amt.try_into().expect("amt out of range");
        let new_frame_offset = self
            .current_frame
            .cursor_decompressed_frame_offset
            .checked_add(amt)
            .expect("frame offset out of range");
        let decompressed_len: u64 = self
            .current_frame
            .decompressed
            .len()
            .try_into()
            .expect("decompressed.len() out of range");
        assert!(new_frame_offset <= decompressed_len);

        self.current_frame.cursor_decompressed_frame_offset = new_frame_offset;
    }
}

struct ZstdFrameInfo {
    compressed_range: Range<u64>,
    decompressed_range: Range<u64>,
}

struct ZstdCurrentFrame {
    decoder: zstd::stream::raw::Decoder<'static>,
    frame_index: usize,
    cursor_decompressed_frame_offset: u64,
    compressed_offset: u64,
    decompressed_offset: u64,
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
            cursor_decompressed_frame_offset: 0,
            compressed_offset: 0,
            compressed_length: 0,
            decompressed_offset: 0,
            decompressed: vec![],
            state: ZstdCurrentFrameState::InProgress,
        })
    }

    fn unread(&self) -> anyhow::Result<&[u8]> {
        let cursor_offset: usize = self.cursor_decompressed_frame_offset.try_into()?;
        let unread = &self.decompressed[cursor_offset..];
        Ok(unread)
    }

    fn finish(&mut self) -> anyhow::Result<ZstdFrameInfo> {
        anyhow::ensure!(
            matches!(self.state, ZstdCurrentFrameState::Finished),
            "called .finish(), but frame is not yet finished"
        );

        let decompressed_len: u64 = self.decompressed.len().try_into()?;

        let compressed_start = self.compressed_offset;
        let compressed_end = compressed_start + self.compressed_length;
        let decompressed_start = self.decompressed_offset;
        let decompressed_end = decompressed_start + decompressed_len;

        let compressed_range = compressed_start..compressed_end;
        let decompressed_range = decompressed_start..decompressed_end;

        let Self {
            decoder,
            frame_index,
            cursor_decompressed_frame_offset,
            compressed_offset,
            decompressed_offset,
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
}

#[derive(Debug, Clone, Copy)]
enum ZstdCurrentFrameState {
    InProgress,
    Finished,
    Failed,
}
