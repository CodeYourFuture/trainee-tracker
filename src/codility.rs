use axum::{Json, body::Body, extract::Request};
use futures::StreamExt;
use http::HeaderMap;

use crate::Error;

// See Callback Authentication section of https://codility.com/api-documentation/#/operations/tests_invite_create
pub async fn verify_webhook(
    header_map: HeaderMap,
    body: Request<Body>,
) -> Result<Json<bool>, Error> {
    let Some(auth_header) = header_map.get("authorization") else {
        return Err(Error::UserFacing("Missing authorization header".to_owned()));
    };
    let Some(token) = auth_header.as_bytes().strip_prefix(b"Bearer ") else {
        return Err(Error::UserFacing("Invalid authorization header".to_owned()));
    };
    let Some(posted_checksum) = header_map.get("checksum") else {
        return Err(Error::UserFacing("Missing checksum header".to_owned()));
    };

    let mut hasher = md5::Context::new();

    let mut data_stream = body.into_body().into_data_stream();
    while let Some(chunk) = data_stream.next().await {
        if let Ok(chunk) = chunk {
            hasher.consume(chunk);
        } else {
            return Err(Error::UserFacing("Failed to read request body".to_owned()));
        }
    }
    hasher.consume(token);
    let digest = hasher.finalize();
    let formatted_digest = format!("{:x}", digest);
    Ok(Json(
        formatted_digest.as_bytes() == posted_checksum.as_bytes(),
    ))
}
