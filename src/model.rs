//! Closed vocabularies: one mapping defines Rust variants, JSON and display names.
macro_rules! text_enum {
    ($(#[$enum_meta:meta])* $vis:vis enum $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        $(#[$enum_meta])*
        $vis enum $name {
            $($(#[$variant_meta])* #[serde(rename = $text)] $variant),+
        }
        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl std::str::FromStr for $name {
            type Err = anyhow::Error;
            fn from_str(value: &str) -> anyhow::Result<Self> {
                match value {
                    $($text => Ok(Self::$variant)),+,
                    _ => anyhow::bail!("不明な{}: {}", stringify!($name), value),
                }
            }
        }
    };
}
pub(crate) use text_enum;
