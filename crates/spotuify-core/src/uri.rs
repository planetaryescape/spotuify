//! Canonical resource identifiers shared by every spotuify layer.
//!
//! Provider-specific input normalization belongs in provider adapters. This
//! module accepts only the canonical `<scheme>:<kind>:<id>` representation.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use crate::MediaKind;
use serde::{Deserialize, Serialize};

/// Extensible URI resource namespace.
///
/// Schemes use the same conservative grammar as provider registry IDs. The
/// owned form lets multiple instances of one adapter use independent resource
/// namespaces (`fake-a`, `fake-b`) without teaching core about every adapter.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct UriScheme(Cow<'static, str>);

impl UriScheme {
    /// Compatibility constants for the built-in resource namespaces.
    #[allow(non_upper_case_globals)]
    pub const Spotify: Self = Self(Cow::Borrowed("spotify"));
    #[allow(non_upper_case_globals)]
    pub const Fake: Self = Self(Cow::Borrowed("fake"));

    pub fn new(value: impl Into<String>) -> Result<Self, UriSchemeError> {
        let value = value.into();
        let mut bytes = value.bytes();
        let valid = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid {
            return Err(UriSchemeError { value });
        }
        Ok(Self(Cow::Owned(value)))
    }

    pub fn label(&self) -> &str {
        self.0.as_ref()
    }
}

impl<'de> Deserialize<'de> for UriScheme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl FromStr for UriScheme {
    type Err = UriSchemeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UriSchemeError {
    pub value: String,
}

impl fmt::Display for UriSchemeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid URI scheme `{}`; expected lowercase ASCII letters, digits, or hyphens",
            self.value
        )
    }
}

impl std::error::Error for UriSchemeError {}

impl fmt::Display for UriScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Why a resource URI could not be parsed as a canonical identifier.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UriError {
    InvalidShape,
    UnsupportedScheme(String),
    UnsupportedKind(String),
    UnexpectedKind {
        expected: MediaKind,
        actual: MediaKind,
    },
    EmptyId,
    InvalidId(String),
}

impl fmt::Display for UriError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidShape => {
                f.write_str("resource URI must have the form <scheme>:<kind>:<id>")
            }
            Self::UnsupportedScheme(scheme) => {
                write!(f, "unsupported resource URI scheme `{scheme}`")
            }
            Self::UnsupportedKind(kind) => write!(f, "unsupported resource kind `{kind}`"),
            Self::UnexpectedKind { expected, actual } => {
                write!(f, "expected resource kind `{expected}`, got `{actual}`")
            }
            Self::EmptyId => f.write_str("resource URI id must not be empty"),
            Self::InvalidId(id) => write!(f, "resource URI id `{id}` is not canonical"),
        }
    }
}

impl std::error::Error for UriError {}

/// Canonical resource identifier.
///
/// The leading scheme is the provider-owned resource namespace used for
/// deterministic routing across any registered adapters.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResourceUri {
    scheme: UriScheme,
    kind: MediaKind,
    id: String,
}

impl ResourceUri {
    /// Build a canonical resource URI from typed components.
    pub fn new(
        scheme: UriScheme,
        kind: MediaKind,
        id: impl Into<String>,
    ) -> Result<Self, UriError> {
        let id = id.into();
        validate_bare_id(&id)?;
        Ok(Self { scheme, kind, id })
    }

    /// Build a canonical Spotify resource URI from a kind and bare ID.
    pub fn spotify(kind: MediaKind, id: impl Into<String>) -> Result<Self, UriError> {
        Self::new(UriScheme::Spotify, kind, id)
    }

    /// Accept either a canonical Spotify URI of `kind` or a bare Spotify ID.
    pub fn spotify_from_uri_or_id(kind: MediaKind, input: &str) -> Result<Self, UriError> {
        match Self::parse(input) {
            Ok(resource) if resource.scheme() != &UriScheme::Spotify => {
                Err(UriError::UnsupportedScheme(resource.scheme().to_string()))
            }
            Ok(resource) if resource.kind() == kind => Ok(resource),
            Ok(resource) => Err(UriError::UnexpectedKind {
                expected: kind,
                actual: resource.kind(),
            }),
            Err(UriError::InvalidShape) => Self::spotify(kind, input),
            Err(err) => Err(err),
        }
    }

    /// Parse a strict canonical `<scheme>:<kind>:<id>` resource URI.
    pub fn parse(input: &str) -> Result<Self, UriError> {
        let mut parts = input.split(':');
        let (Some(scheme), Some(kind), Some(id), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(UriError::InvalidShape);
        };
        let scheme =
            UriScheme::new(scheme).map_err(|err| UriError::UnsupportedScheme(err.value))?;
        let kind = MediaKind::from_str(kind).map_err(|err| UriError::UnsupportedKind(err.value))?;
        Self::new(scheme, kind, id)
    }

    pub fn scheme(&self) -> &UriScheme {
        &self.scheme
    }

    pub fn kind(&self) -> MediaKind {
        self.kind.clone()
    }

    pub fn bare_id(&self) -> &str {
        &self.id
    }

    pub fn as_uri(&self) -> String {
        self.to_string()
    }
}

fn validate_bare_id(id: &str) -> Result<(), UriError> {
    if id.is_empty() {
        return Err(UriError::EmptyId);
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
    {
        return Err(UriError::InvalidId(id.to_string()));
    }
    Ok(())
}

impl fmt::Display for ResourceUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.scheme, self.kind, self.id)
    }
}

impl FromStr for ResourceUri {
    type Err = UriError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        Self::parse(input)
    }
}

impl Serialize for ResourceUri {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.as_uri())
    }
}

impl<'de> Deserialize<'de> for ResourceUri {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

macro_rules! spotify_id_newtype {
    ($name:ident, $kind:ident, $kind_label:literal) => {
        #[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap a bare Spotify ID without validation.
            ///
            /// This preserves the original ID-newtype API. Prefer
            /// [`Self::from_uri`] when handling an external full URI.
            ///
            /// Unvalidated escape hatch: the input is stored verbatim, so a
            /// caller can construct an ID whose [`Self::to_uri`] output does not
            /// round-trip through [`ResourceUri::parse`] (e.g. an ID containing
            /// a colon or whitespace). Validation is intentionally omitted to
            /// keep this API-preserving; use [`Self::from_uri`] for anything
            /// coming from outside the process.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// Parse a canonical Spotify URI of this resource kind.
            pub fn from_uri(uri: &str) -> Option<Self> {
                let parsed = ResourceUri::parse(uri).ok()?;
                (parsed.scheme() == &UriScheme::Spotify && parsed.kind() == MediaKind::$kind)
                    .then(|| Self(parsed.id))
            }

            /// Return the bare provider ID.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Return the full canonical Spotify URI.
            pub fn to_uri(&self) -> String {
                format!("spotify:{}:{}", $kind_label, self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

spotify_id_newtype!(TrackId, Track, "track");
spotify_id_newtype!(ArtistId, Artist, "artist");
spotify_id_newtype!(AlbumId, Album, "album");
spotify_id_newtype!(PlaylistId, Playlist, "playlist");

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use crate::{
        AlbumId, ArtistId, MediaKind, PlaylistId, ResourceUri, TrackId, UriError, UriScheme,
    };

    #[test]
    fn parses_every_supported_canonical_resource_kind() {
        let cases = [
            ("spotify:track:track-1", MediaKind::Track, "track-1"),
            ("spotify:episode:episode-1", MediaKind::Episode, "episode-1"),
            ("spotify:show:show-1", MediaKind::Show, "show-1"),
            ("spotify:album:album-1", MediaKind::Album, "album-1"),
            ("spotify:artist:artist-1", MediaKind::Artist, "artist-1"),
            (
                "spotify:playlist:playlist-1",
                MediaKind::Playlist,
                "playlist-1",
            ),
        ];

        for (input, expected_kind, expected_id) in cases {
            let uri = ResourceUri::parse(input).expect("canonical URI should parse");
            assert_eq!(uri.scheme(), &UriScheme::Spotify);
            assert_eq!(uri.kind(), expected_kind);
            assert_eq!(uri.bare_id(), expected_id);
        }
    }

    #[test]
    fn fake_provider_has_an_independent_canonical_namespace() {
        let uri = ResourceUri::parse("fake:track:shared").unwrap();
        assert_eq!(uri.scheme(), &UriScheme::Fake);
        assert_eq!(uri.kind(), MediaKind::Track);
        assert_eq!(uri.bare_id(), "shared");
        assert_eq!(uri.as_uri(), "fake:track:shared");
    }

    #[test]
    fn provider_instances_can_have_distinct_dynamic_namespaces() {
        let first = ResourceUri::parse("fake-a:track:shared").unwrap();
        let second = ResourceUri::parse("fake-b:track:shared").unwrap();
        assert_ne!(first, second);
        assert_eq!(first.scheme().label(), "fake-a");
        assert_eq!(second.scheme().label(), "fake-b");
    }

    #[test]
    fn rejects_malformed_and_non_canonical_resource_uris() {
        let cases = [
            ("", UriError::InvalidShape),
            ("spotify:track", UriError::InvalidShape),
            ("spotify:track:id:extra", UriError::InvalidShape),
            ("spotify:user:alice:playlist:mix", UriError::InvalidShape),
            (
                "Spotify:track:id",
                UriError::UnsupportedScheme("Spotify".to_string()),
            ),
            (
                "spotify:video:id",
                UriError::UnsupportedKind("video".to_string()),
            ),
            (
                "spotify:Track:id",
                UriError::UnsupportedKind("Track".to_string()),
            ),
            ("spotify:track:", UriError::EmptyId),
            (
                "spotify:track:id?si=junk",
                UriError::InvalidId("id?si=junk".to_string()),
            ),
            (
                "spotify:track:two words",
                UriError::InvalidId("two words".to_string()),
            ),
            (
                "spotify:track:id/child",
                UriError::InvalidId("id/child".to_string()),
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(ResourceUri::parse(input), Err(expected), "input: {input}");
        }
    }

    #[test]
    fn canonical_uri_round_trips_through_output_interfaces() {
        let input = "spotify:episode:episode_1-.~";
        let uri = ResourceUri::parse(input).expect("canonical URI should parse");

        assert_eq!(uri.as_uri(), input);
        assert_eq!(uri.to_string(), input);
        assert_eq!(input.parse::<ResourceUri>(), Ok(uri));
    }

    #[test]
    fn constructors_validate_bare_ids_and_emit_canonical_uris() {
        let generic = ResourceUri::new(UriScheme::Spotify, MediaKind::Album, "album-1")
            .expect("valid bare ID should construct");
        let spotify = ResourceUri::spotify(MediaKind::Track, "track_1")
            .expect("valid Spotify ID should construct");

        assert_eq!(generic.as_uri(), "spotify:album:album-1");
        assert_eq!(spotify.as_uri(), "spotify:track:track_1");
        assert_eq!(
            ResourceUri::spotify(MediaKind::Track, "track?si=junk"),
            Err(UriError::InvalidId("track?si=junk".to_string()))
        );
        assert_eq!(
            ResourceUri::new(UriScheme::Spotify, MediaKind::Playlist, ""),
            Err(UriError::EmptyId)
        );
    }

    #[test]
    fn spotify_uri_or_id_preserves_canonical_uris_and_rejects_wrong_or_malformed_kinds() {
        assert_eq!(
            ResourceUri::spotify_from_uri_or_id(MediaKind::Playlist, "playlist-1")
                .unwrap()
                .as_uri(),
            "spotify:playlist:playlist-1"
        );
        assert_eq!(
            ResourceUri::spotify_from_uri_or_id(MediaKind::Playlist, "spotify:playlist:playlist-1")
                .unwrap()
                .as_uri(),
            "spotify:playlist:playlist-1"
        );
        assert!(matches!(
            ResourceUri::spotify_from_uri_or_id(MediaKind::Playlist, "spotify:track:track-1"),
            Err(UriError::UnexpectedKind { .. })
        ));
        assert_eq!(
            ResourceUri::spotify_from_uri_or_id(MediaKind::Playlist, "fake:playlist:playlist-1"),
            Err(UriError::UnsupportedScheme("fake".to_string()))
        );
        assert!(
            ResourceUri::spotify_from_uri_or_id(MediaKind::Playlist, "spotify:playlist:").is_err()
        );
    }

    #[test]
    fn typed_ids_keep_their_api_and_inherit_strict_uri_validation() {
        let track = TrackId::from_uri("spotify:track:track-1").expect("track URI should parse");
        let artist =
            ArtistId::from_uri("spotify:artist:artist-1").expect("artist URI should parse");
        let album = AlbumId::from_uri("spotify:album:album-1").expect("album URI should parse");
        let playlist =
            PlaylistId::from_uri("spotify:playlist:playlist-1").expect("playlist URI should parse");

        assert_eq!(track.as_str(), "track-1");
        assert_eq!(track.to_uri(), "spotify:track:track-1");
        assert_eq!(artist.to_uri(), "spotify:artist:artist-1");
        assert_eq!(album.to_uri(), "spotify:album:album-1");
        assert_eq!(playlist.to_uri(), "spotify:playlist:playlist-1");
        assert!(TrackId::from_uri("spotify:artist:track-1").is_none());
        assert!(TrackId::from_uri("spotify:track:track-1?si=junk").is_none());
        assert!(TrackId::from_uri("spotify:track:two words").is_none());
    }
}
