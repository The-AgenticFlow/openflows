//! Custom Axum extractors that produce structured ApiErrorEnvelope on validation or parse failures.

use crate::{
    error::ManagerError,
    middleware::request_id::{RequestId, REQUEST_ID_HEADER},
};
use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        FromRequest, FromRequestParts, OptionalFromRequest, Request,
    },
    http::{request::Parts, HeaderValue},
    response::{IntoResponse, Response},
};

/// Custom JSON extractor that wraps axum::Json and returns ApiErrorEnvelope on rejection.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppJson<T>(pub T);

impl<S, T> FromRequest<S> for AppJson<T>
where
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let req_id = req.extensions().get::<RequestId>().cloned();
        match axum::Json::<T>::from_request(req, state).await {
            Ok(value) => Ok(Self(value.0)),
            Err(rejection) => {
                let err = ManagerError::InvalidRequest(rejection.body_text());
                let (status, envelope) = err.to_api_response(req_id.as_ref());
                let mut resp = (status, envelope).into_response();
                if let Some(id) = req_id {
                    if let Ok(hv) = HeaderValue::from_str(id.as_str()) {
                        resp.headers_mut().insert(REQUEST_ID_HEADER, hv);
                    }
                }
                Err(resp)
            }
        }
    }
}

impl<S, T> OptionalFromRequest<S> for AppJson<T>
where
    axum::Json<T>: OptionalFromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Option<Self>, Self::Rejection> {
        let req_id = req.extensions().get::<RequestId>().cloned();
        match axum::Json::<T>::from_request(req, state).await {
            Ok(value) => Ok(value.map(|j| Self(j.0))),
            Err(rejection) => {
                let err = ManagerError::InvalidRequest(rejection.body_text());
                let (status, envelope) = err.to_api_response(req_id.as_ref());
                let mut resp = (status, envelope).into_response();
                if let Some(id) = req_id {
                    if let Ok(hv) = HeaderValue::from_str(id.as_str()) {
                        resp.headers_mut().insert(REQUEST_ID_HEADER, hv);
                    }
                }
                Err(resp)
            }
        }
    }
}

/// Custom Query extractor that wraps axum::extract::Query and returns ApiErrorEnvelope on rejection.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for AppQuery<T>
where
    axum::extract::Query<T>: FromRequestParts<S, Rejection = QueryRejection>,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let req_id = parts.extensions.get::<RequestId>().cloned();
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(value) => Ok(Self(value.0)),
            Err(rejection) => {
                let err = ManagerError::InvalidRequest(rejection.body_text());
                let (status, envelope) = err.to_api_response(req_id.as_ref());
                let mut resp = (status, envelope).into_response();
                if let Some(id) = req_id {
                    if let Ok(hv) = HeaderValue::from_str(id.as_str()) {
                        resp.headers_mut().insert(REQUEST_ID_HEADER, hv);
                    }
                }
                Err(resp)
            }
        }
    }
}
