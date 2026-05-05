use thiserror::Error;

#[derive(Error, Debug)]
pub enum A5CogError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("object_store error: {0}")]
    ObjectStore(#[from] object_store::Error),

    #[error("async-tiff error: {0}")]
    Tiff(#[from] async_tiff::error::AsyncTiffError),

    #[error("proj error: {0}")]
    Proj(String),

    #[error("a5 error: {0}")]
    A5(String),

    #[error("missing geokey: {0}")]
    MissingGeoKey(&'static str),

    #[error("unsupported configuration: {0}")]
    Unsupported(String),

    #[error("invalid input: {0}")]
    Invalid(String),

    #[error("url parse error: {0}")]
    Url(#[from] url::ParseError),
}

pub type Result<T> = std::result::Result<T, A5CogError>;

impl From<proj4rs::errors::Error> for A5CogError {
    fn from(e: proj4rs::errors::Error) -> Self {
        Self::Proj(e.to_string())
    }
}
