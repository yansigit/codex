//! HTTP responses retain application authorization while their bodies are consumed.

use std::ops::Deref;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

use bytes::Bytes;
use futures::FutureExt;
use futures::Stream;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream;
use serde::de::DeserializeOwned;

use crate::HttpError;
use crate::NetworkPermit;
use crate::NetworkPolicy;

#[derive(Debug)]
pub struct HttpResponse {
    inner: reqwest::Response,
    permit: NetworkPermit,
    redact_error_url: bool,
}

impl HttpResponse {
    /// Preserves HTTP frames, including gRPC trailers, while enforcing revocation.
    pub fn into_http_response(
        self,
    ) -> http::Response<impl http_body::Body<Data = Bytes, Error = HttpError> + Send> {
        let response: http::Response<reqwest::Body> = self.inner.into();
        let revoked = self.permit.clone();
        response.map(|body| PolicyBody {
            body: Some(Box::pin(body)),
            permit: self.permit,
            revoked: async move { revoked.revoked().await }.boxed(),
            redact_error_url: self.redact_error_url,
        })
    }
    pub(crate) fn new(inner: reqwest::Response, permit: NetworkPermit) -> Self {
        Self {
            inner,
            permit,
            redact_error_url: false,
        }
    }

    pub(crate) fn without_error_urls(mut self) -> Self {
        self.redact_error_url = true;
        self
    }

    pub async fn bytes(self) -> Result<Bytes, HttpError> {
        Box::pin(self.permit.run(self.inner.bytes()))
            .await?
            .map_err(|error| response_error(error, self.redact_error_url))
    }

    pub async fn text(self) -> Result<String, HttpError> {
        Box::pin(self.permit.run(self.inner.text()))
            .await?
            .map_err(|error| response_error(error, self.redact_error_url))
    }

    pub async fn json<T: DeserializeOwned>(self) -> Result<T, HttpError> {
        Box::pin(self.permit.run(self.inner.json()))
            .await?
            .map_err(|error| response_error(error, self.redact_error_url))
    }

    pub async fn chunk(&mut self) -> Result<Option<Bytes>, HttpError> {
        Box::pin(self.permit.run(self.inner.chunk()))
            .await?
            .map_err(|error| response_error(error, self.redact_error_url))
    }

    pub fn error_for_status(self) -> Result<Self, HttpError> {
        self.permit.check()?;
        Ok(Self {
            inner: self
                .inner
                .error_for_status()
                .map_err(|error| response_error(error, self.redact_error_url))?,
            permit: self.permit,
            redact_error_url: self.redact_error_url,
        })
    }

    pub fn error_for_status_ref(&self) -> Result<&Self, HttpError> {
        self.permit.check()?;
        self.inner
            .error_for_status_ref()
            .map_err(|error| response_error(error, self.redact_error_url))?;
        Ok(self)
    }

    pub fn bytes_stream(self) -> impl Stream<Item = Result<Bytes, HttpError>> + Send + Unpin {
        Box::pin(stream::try_unfold(
            (
                Box::pin(self.inner.bytes_stream()),
                self.permit,
                self.redact_error_url,
            ),
            |(mut body, permit, redact_error_url)| async move {
                let next = permit.run(body.next()).await?;
                next.transpose()
                    .map(|next| next.map(|bytes| (bytes, (body, permit, redact_error_url))))
                    .map_err(|error| response_error(error, redact_error_url))
            },
        ))
    }
}

struct PolicyBody<B> {
    body: Option<Pin<Box<B>>>,
    permit: NetworkPermit,
    revoked: BoxFuture<'static, ()>,
    redact_error_url: bool,
}

impl<B: http_body::Body<Data = Bytes, Error = reqwest::Error>> http_body::Body for PolicyBody<B> {
    type Data = Bytes;
    type Error = HttpError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, HttpError>>> {
        let body = self.get_mut();
        let Some(inner) = body.body.as_mut() else {
            return Poll::Ready(None);
        };
        if body.permit.check().is_err() || body.revoked.poll_unpin(cx).is_ready() {
            body.body = None;
            return Poll::Ready(Some(Err(crate::NetworkPolicyDenied::Revoked.into())));
        }
        inner.as_mut().poll_frame(cx).map(|frame| {
            frame.map(|frame| frame.map_err(|error| response_error(error, body.redact_error_url)))
        })
    }

    fn is_end_stream(&self) -> bool {
        self.body
            .as_ref()
            .is_none_or(http_body::Body::is_end_stream)
    }
    fn size_hint(&self) -> http_body::SizeHint {
        self.body
            .as_ref()
            .map_or_else(http_body::SizeHint::default, http_body::Body::size_hint)
    }
}

impl Deref for HttpResponse {
    type Target = reqwest::Response;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl From<reqwest::Response> for HttpResponse {
    fn from(inner: reqwest::Response) -> Self {
        let permit = NetworkPolicy::unmanaged()
            .acquire(inner.url())
            .unwrap_or_else(|_| unreachable!("unmanaged policy permits every URL"));
        Self::new(inner, permit)
    }
}

fn response_error(error: reqwest::Error, redact_url: bool) -> HttpError {
    if redact_url {
        error.without_url().into()
    } else {
        error.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::ResponseBuilderExt;

    const PRIVATE_URL: &str = "https://storage.example.test/file?signature=private";

    fn failed_response() -> HttpResponse {
        let body = reqwest::Body::wrap_stream(futures::stream::once(async {
            Err::<Bytes, _>(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "body timed out",
            ))
        }));
        HttpResponse::from(
            http::Response::builder()
                .url(PRIVATE_URL.parse().expect("valid URL"))
                .body(body)
                .expect("valid response"),
        )
        .without_error_urls()
    }

    #[tokio::test]
    async fn private_body_errors_keep_timeout_classification_without_the_url() {
        use http_body::Body as _;

        let mut framed_body = std::pin::pin!(failed_response().into_http_response().into_body());
        let frame_error = futures::future::poll_fn(|cx| framed_body.as_mut().poll_frame(cx))
            .await
            .expect("body frame")
            .expect_err("body failure");
        let errors = [
            frame_error,
            failed_response().bytes().await.expect_err("body failure"),
            failed_response().text().await.expect_err("body failure"),
            failed_response()
                .json::<serde_json::Value>()
                .await
                .expect_err("body failure"),
            failed_response().chunk().await.expect_err("body failure"),
            failed_response()
                .bytes_stream()
                .next()
                .await
                .expect("stream item")
                .expect_err("body failure"),
        ];
        for error in errors {
            assert!(error.is_timeout(), "{error:?}");
            assert!(!format!("{error:?}").contains("private"));
            assert!(!error.to_string().contains("storage.example.test"));
        }
    }

    #[test]
    fn private_status_errors_preserve_status_without_the_url() {
        let response = HttpResponse::from(
            http::Response::builder()
                .url(PRIVATE_URL.parse().expect("valid URL"))
                .status(403)
                .body("")
                .expect("valid response"),
        )
        .without_error_urls();
        let error = response.error_for_status_ref().expect_err("forbidden");
        assert_eq!(error.status(), Some(reqwest::StatusCode::FORBIDDEN));
        assert!(!format!("{error:?}").contains("private"));
        let error = response.error_for_status().expect_err("forbidden");
        assert_eq!(error.status(), Some(reqwest::StatusCode::FORBIDDEN));
        assert!(!format!("{error:?}").contains("private"));
    }
}

impl<T: Into<reqwest::Body>> From<http::Response<T>> for HttpResponse {
    fn from(response: http::Response<T>) -> Self {
        reqwest::Response::from(response).into()
    }
}
