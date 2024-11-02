use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::TryStreamExt;
use reqwest_streams::error::{StreamBodyError, StreamBodyKind};
use reqwest_streams::StreamBodyResult;
use tokio_util::io::StreamReader;

const INITIAL_CAPACITY: usize = 8 * 1024;

#[async_trait]
pub trait LinesStreamResponse {
    fn lines_stream<'a, 'b>(
        self,
        max_obj_len: usize,
    ) -> BoxStream<'b, StreamBodyResult<String>>;
    fn lines_stream_with_capacity<'a, 'b>(
        self,
        max_obj_len: usize,
        buf_capacity: usize,
    ) -> BoxStream<'b, StreamBodyResult<String>>;
}

#[async_trait]
impl LinesStreamResponse for reqwest::Response {
    fn lines_stream<'a, 'b>(
        self,
        max_obj_len: usize,
    ) -> BoxStream<'b, StreamBodyResult<String>> {
        self.lines_stream_with_capacity(max_obj_len, INITIAL_CAPACITY)
    }

    fn lines_stream_with_capacity<'a, 'b>(
        self,
        max_obj_len: usize,
        buf_capacity: usize,
    ) -> BoxStream<'b, StreamBodyResult<String>> {
        let reader = StreamReader::new(
            self.bytes_stream()
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err)),
        );
        let codec = tokio_util::codec::LinesCodec::new_with_max_length(max_obj_len);
        let frames_reader =
            tokio_util::codec::FramedRead::with_capacity(reader, codec, buf_capacity);
        let res = Box::pin(frames_reader.into_stream().map_err(|err| {
            StreamBodyError::new(StreamBodyKind::CodecError, Some(Box::new(err)), None)
        }));
        res
    }
}
