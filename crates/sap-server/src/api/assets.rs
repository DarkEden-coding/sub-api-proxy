use axum::extract::OriginalUri;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};

include!(concat!(env!("OUT_DIR"), "/dashboard.rs"));

pub(super) async fn redirect() -> Redirect {
    Redirect::permanent("/sap/")
}

pub(super) async fn serve(OriginalUri(uri): OriginalUri) -> Response {
    let path = uri.path().strip_prefix("/sap").unwrap_or("/");
    let (bytes, mime, cache) = if let Some((bytes, mime)) = dashboard_asset(path) {
        // Vite emits content hashes on its assets. Other public files can change
        // without a new name, so they must be revalidated.
        let immutable = path.starts_with("/assets/")
            && path
                .rsplit_once('.')
                .and_then(|(stem, _)| stem.split_once('-'))
                .is_some_and(|(_, tail)| {
                    tail.split('.').next().is_some_and(|hash| {
                        hash.len() >= 8
                            && hash
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    })
                });
        (
            bytes,
            mime,
            if immutable {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            },
        )
    } else if path.starts_with("/assets/") || path.starts_with("/fonts/") {
        return StatusCode::NOT_FOUND.into_response();
    } else {
        let (bytes, mime) = dashboard_asset("/index.html").expect("dashboard index embedded at build time");
        (bytes, mime, "no-cache")
    };
    ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache)], bytes).into_response()
}
