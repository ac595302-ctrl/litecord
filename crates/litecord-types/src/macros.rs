/// Defines a fieldless enum with a stable snake_case string form used for
/// SQLite storage and serde. Adding variants is backwards compatible; renaming
/// the string of an existing variant is a schema migration.
macro_rules! str_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident { $( $(#[$vmeta:meta])* $variant:ident => $s:literal ),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
        pub enum $name {
            $( $(#[$vmeta])* #[serde(rename = $s)] $variant ),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }

            pub fn parse(s: &str) -> Result<Self, $crate::ValidationError> {
                match s {
                    $($s => Ok($name::$variant),)+
                    _ => Err($crate::ValidationError::Parse { what: stringify!($name), input: s.to_owned() }),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
