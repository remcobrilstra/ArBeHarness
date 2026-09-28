//! Plumbing shared by every HTTP adapter: sending a request with
//! cancellation and status checking, turning a response body into a
//! line/chunk stream, and making any event stream cancellable.

use std::time::Duration;

use arbe_core::ProviderError;
use futures_util::StreamExt;
use futures_util::stream::{BoxStream, Stream};
use tokio_util::sync::CancellationToken;

use crate::ProviderEvent;
use crate::error_map::{map_http_response, map_transport_error};
use crate::utf8_buffer::Utf8ChunkBuffer;

/// Time allowed to establish a connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Longest a response may go *silent* (between bytes, not in total).
/// Generous because reasoning models can think for minutes before their
/// first token; without it, a stalled connection would hang a turn forever.
pub const READ_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The HTTP client every adapter uses: connect and idle-read timeouts,
/// no overall request timeout (a long answer is fine as long as it keeps
/// arriving).
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_IDLE_TIMEOUT)
        .build()
        // Only fails if the TLS backend can't initialize, which would make
        // every provider unusable anyway; the default client has the same
        // failure mode (it panics too).
        .expect("failed to initialize the HTTP client")
}

/// Sends `request`, racing it against `cancel`. A non-2xx status is read
/// and mapped into the `ProviderError` taxonomy here, so an adapter only
/// ever sees a successful response.
pub async fn send(
    request: reqwest::RequestBuilder,
    cancel: &CancellationToken,
) -> Result<reqwest::Response, ProviderError> {
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        response = request.send() => response.map_err(map_transport_error)?,
    };
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let headers = response.headers().clone();
    let body = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
        body = response.text() => body.map_err(map_transport_error)?,
    };
    Err(map_http_response(status, &headers, &body))
}

/// Applies configured extra headers to a request.
pub(crate) fn with_extra_headers(
    mut request: reqwest::RequestBuilder,
    headers: &[(String, String)],
) -> reqwest::RequestBuilder {
    for (name, value) in headers {
        request = request.header(name, value);
    }
    request
}

/// A response body as a stream of UTF-8 text chunks. Multi-byte characters
/// split across network chunks are held back until complete (see
/// [`Utf8ChunkBuffer`]).
pub fn text_chunks(
    response: reqwest::Response,
) -> impl Stream<Item = Result<String, ProviderError>> + Send + 'static {
    async_stream::stream! {
        let mut utf8 = Utf8ChunkBuffer::new();
        let mut bytes = response.bytes_stream();
        while let Some(chunk) = bytes.next().await {
            match chunk {
                Ok(chunk) => yield Ok(utf8.push(&chunk)),
                Err(e) => {
                    yield Err(map_transport_error(e));
                    return;
                }
            }
        }
    }
}

/// Wraps an event stream so that `cancel` firing ends it promptly with a
/// final `Err(ProviderError::Cancelled)`. Dropping the inner stream drops
/// the HTTP response, which closes the connection, so the provider stops
/// generating (and billing) too.
pub fn cancellable<S>(
    inner: S,
    cancel: CancellationToken,
) -> BoxStream<'static, Result<ProviderEvent, ProviderError>>
where
    S: Stream<Item = Result<ProviderEvent, ProviderError>> + Send + 'static,
{
    Box::pin(async_stream::stream! {
        let mut inner = Box::pin(inner);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    yield Err(ProviderError::Cancelled);
                    return;
                }
                item = inner.next() => match item {
                    Some(item) => {
                        let is_err = item.is_err();
                        yield item;
                        if is_err {
                            return;
                        }
                    }
                    None => return,
                },
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellable_passes_events_through_until_the_end() {
        let inner = futures_util::stream::iter(vec![
            Ok(ProviderEvent::TextDelta("a".into())),
            Ok(ProviderEvent::TextDelta("b".into())),
        ]);
        let out: Vec<_> = cancellable(inner, CancellationToken::new()).collect().await;
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|e| e.is_ok()));
    }

    #[tokio::test]
    async fn cancellable_ends_with_cancelled_once_the_token_fires() {
        let cancel = CancellationToken::new();
        // An inner stream that never yields: only cancellation can end it.
        let inner = futures_util::stream::pending::<Result<ProviderEvent, ProviderError>>();
        let mut stream = cancellable(inner, cancel.clone());
        cancel.cancel();
        assert!(matches!(
            stream.next().await,
            Some(Err(ProviderError::Cancelled))
        ));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn cancellable_stops_after_the_first_error() {
        let inner = futures_util::stream::iter(vec![
            Err(ProviderError::Internal("boom".into())),
            Ok(ProviderEvent::TextDelta("after".into())),
        ]);
        let out: Vec<_> = cancellable(inner, CancellationToken::new()).collect().await;
        assert_eq!(out.len(), 1);
    }
}
