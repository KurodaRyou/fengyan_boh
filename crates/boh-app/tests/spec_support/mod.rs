//! HTTP 锁定测试的辅助代码：只经 `boh_app::test_router` 返回的 `Router` 访问系统。

use std::convert::Infallible;
use std::error::Error;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, Response, StatusCode, header};
use serde_json::Value;

pub struct JsonReply {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Value,
}

/// 发送 `GET uri`，响应体按 JSON 解析。
pub async fn get(router: Router, uri: &str) -> Result<JsonReply, Box<dyn Error>> {
    let request = Request::get(uri).body(Body::empty())?;
    let response = call(router, request).await;
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|value| value.to_str())
        .transpose()?
        .map(str::to_owned);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body = serde_json::from_slice(&bytes)?;
    Ok(JsonReply {
        status,
        content_type,
        body,
    })
}

/// axum 不重新导出 tower 的 `Service`；经它的父 trait `axum::ServiceExt` 调用，测试不新增依赖。
async fn call<S>(mut service: S, request: Request<Body>) -> Response<Body>
where
    S: axum::ServiceExt<Request<Body>, Response = Response<Body>, Error = Infallible>,
{
    let Ok(()) = std::future::poll_fn(|cx| service.poll_ready(cx)).await;
    let Ok(response) = service.call(request).await;
    response
}
