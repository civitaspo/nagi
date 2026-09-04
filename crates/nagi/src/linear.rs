//! Linear integration boundaries.

pub mod oauth;

pub mod credentials;

#[cfg(any(test, target_os = "macos"))]
pub mod read;

#[cfg(test)]
mod polling;

/// Coarse failures for the Linear read verifier. No provider response or
/// deployment-local value is retained in an error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadContractError {
    /// The live provider operation is available only on the supported host.
    UnsupportedPlatform,
    /// The local binding or access lease was invalid.
    Configuration,
    /// The P0-04 credential lease could not be acquired or refreshed.
    Credential(credentials::CredentialError),
    /// The bounded HTTPS client could not be configured.
    ClientConfiguration,
    /// The request failed before a response was received.
    RequestFailed,
    /// The provider response exceeded the parser bound.
    ResponseTooLarge,
    /// The response did not contain exactly one application/json media type.
    ContentType,
    /// The response omitted or malformed one of the required rate headers.
    RateLimitHeaders,
    /// The HTTP status was not a successful GraphQL response.
    HttpStatus,
    /// The GraphQL response contained an error or invalid JSON shape.
    GraphqlResponse,
    /// The authenticated viewer did not satisfy the required app-actor fields.
    ActorIdentityMismatch,
    /// The exact setup issue/team/workspace relationship did not match.
    RelationshipMismatch,
    /// The Issue or Comment read fields were malformed or incomplete.
    ReadFieldsInvalid,
    /// The Relay pageInfo contract was malformed or exceeded its bound.
    PaginationInvalid,
}

impl std::fmt::Display for ReadContractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::UnsupportedPlatform => "Linear read contract is unsupported on this host",
            Self::Configuration => "Linear read contract configuration is invalid",
            Self::Credential(error) => return error.fmt(formatter),
            Self::ClientConfiguration => "Linear read transport is unavailable",
            Self::RequestFailed => "Linear read request failed",
            Self::ResponseTooLarge => "Linear read response is too large",
            Self::ContentType => "Linear read response content type is invalid",
            Self::RateLimitHeaders => "Linear read rate-limit headers are invalid",
            Self::HttpStatus => "Linear read HTTP response is invalid",
            Self::GraphqlResponse => "Linear read GraphQL response is invalid",
            Self::ActorIdentityMismatch => "Linear app actor identity is invalid",
            Self::RelationshipMismatch => "Linear synthetic issue relationship is invalid",
            Self::ReadFieldsInvalid => "Linear Issue or Comment read fields are invalid",
            Self::PaginationInvalid => "Linear read pagination is invalid",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for ReadContractError {}
